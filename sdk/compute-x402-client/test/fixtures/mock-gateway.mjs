// Unfunded loopback fixture: real wallet signatures and native receipt verification,
// synthetic RPC/settlement. No transaction is broadcast to a public network.
import assert from 'node:assert/strict';
import { createServer } from 'node:http';
import { mkdtempSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { createPrivateKey, sign, randomUUID, createPublicKey, verify } from 'node:crypto';
import { Keypair, VersionedTransaction } from '@solana/web3.js';
import bs58 from 'bs58';
import { SOLANA_GENESIS, NETWORK, USDC, PAYMENT_COMPUTE_UNITS } from '../../index.mjs';
import { CovenantSolanaKeypairWalletProvider } from '../../agentkit.mjs';
import { sha256, stable } from '../../proof.mjs';
import { pathToFileURL } from 'node:url';
import { createKeyPairSignerFromBytes } from '@solana/kit';
import { toFacilitatorSvmSigner } from '@x402/svm';

const tokenProgram = 'TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA';
const encode = v => Buffer.from(JSON.stringify(v)).toString('base64');
const signature = (key, domain, value, field, actor) => {
  const payload = { ...value, [actor]: { display: actor, pubkey: key.publicKey.toBase58() } };
  const raw = stable(payload); const privateKey = createPrivateKey({ key: Buffer.concat([Buffer.from('302e020100300506032b657004220420', 'hex'), key.secretKey.subarray(0,32)]), format:'der', type:'pkcs8' });
  return { [field]: payload, [field+'_json']: raw, signer_pubkey_b58:key.publicKey.toBase58(), signature_b58:bs58.encode(sign(null,Buffer.from(domain+raw),privateKey)) };
};
export const input = id => ({ requestId: id ?? randomUUID(), model:'covenant-test', messages:[{role:'user',content:'Build a useful counter webpage.'}], max_tokens:512, temperature:0.25 });

export async function fixture(options = {}) {
  const home = mkdtempSync(join(tmpdir(),'covenant-x402-test-')); const key = Keypair.generate(), buyer = Keypair.generate(), operator = Keypair.generate(), fee = Keypair.generate(), seller = Keypair.generate();
  let requests = 0, signatures = 0, quotes = 0, sends = 0; const payments = [], confirmed = new Map(), savedQuotes = new Map(); let firstFailure = options.dropFirst; let firstPending = options.pendingFirst;
  const server = createServer(async (req,res) => {
    try {
      requests++;
      let raw=''; for await (const chunk of req) raw+=chunk;
      const body=JSON.parse(raw); res.setHeader('content-type','application/json');
      if(req.url==='/rpc') {
        let result;
        if(body.method==='getGenesisHash') result=options.wrongRpc ? 'wrong-chain' : SOLANA_GENESIS;
        else if(body.method==='getAccountInfo') { const mint=Buffer.alloc(82); mint[44]=6;mint[45]=1;result={context:{slot:1},value:{data:[mint.toString('base64'),'base64'],executable:false,lamports:1461600,owner:tokenProgram,rentEpoch:1,space:82}}; }
        else if(body.method==='getLatestBlockhash') result={context:{slot:1},value:{blockhash:fee.publicKey.toBase58(),lastValidBlockHeight:123456789}};
        else if(body.method==='getTransaction') result=confirmed.get(body.params[0]) ?? null;
        else throw new Error('Unexpected RPC '+body.method);
        res.end(JSON.stringify({jsonrpc:'2.0',id:body.id,result}));return;
      }
      assert.equal(req.url,'/v1/chat/completions'); const digest=sha256(stable(body));const quoteId=req.headers['idempotency-key'];
      const quote=savedQuotes.get(quoteId)??{scheme:'exact',network:NETWORK,asset:USDC,amount:String(options.price?.(body)??1000),payTo:seller.publicKey.toBase58(),maxTimeoutSeconds:90,extra:{feePayer:fee.publicKey.toBase58(),quoteId,requestSha256:digest,expiresAtMs:Date.now()+90000,memo:`covenant.x402.v2:${quoteId}:${digest}`}};
      if(!req.headers['payment-signature']) {
        quotes++; if(options.badQuote) options.badQuote(quote);
        savedQuotes.set(quoteId,quote);
        const required={x402Version:2,resource:{url:base+'/v1/chat/completions',description:'Covenant inference',mimeType:'application/json'},accepts:[quote]};
        res.statusCode=402;res.setHeader('PAYMENT-REQUIRED',encode(required));res.end('{}');return;
      }
      sends++;payments.push(req.headers['payment-signature']);
      if(options.paidGate)await options.paidGate;
      const payment=JSON.parse(Buffer.from(payments.at(-1),'base64')); const transaction=VersionedTransaction.deserialize(Buffer.from(payment.payload.transaction,'base64'));
      assert.deepEqual(payment.accepted,quote);
      const transfer=transaction.message.compiledInstructions.find(ix=>transaction.message.staticAccountKeys[ix.programIdIndex].toBase58()===tokenProgram);
      assert.equal(Buffer.from(transfer.data).readBigUInt64LE(1),BigInt(quote.amount));
      if(options.verifyBridge) {
        const {createBridge}=await import(pathToFileURL(process.env.COVENANT_X402_BRIDGE_PATH).href);
        const signer=toFacilitatorSvmSigner(await createKeyPairSignerFromBytes(fee.secretKey));
        signer.simulateTransaction=async()=>{};
        const bridge=createBridge({signer,config:{network:NETWORK,asset:USDC,payTo:seller.publicKey.toBase58(),feePayer:fee.publicKey.toBase58(),resourceUrl:base+'/v1/chat/completions',priceAtomic:'1000',secret:'unfunded-test-secret-with-more-than-32-chars'},rpc:async(method)=>{assert.equal(method,'getGenesisHash');return SOLANA_GENESIS;}});
        const verified=await bridge.verify({paymentPayload:payment,paymentRequirements:payment.accepted,resourceUrl:base+'/v1/chat/completions'});
        assert.equal(verified.payer,key.publicKey.toBase58());assert.equal(verified.authorization_id,sha256(Buffer.from(payment.payload.transaction,'base64')));
      }
      const limit=transaction.message.compiledInstructions[0];
      assert.equal(transaction.message.staticAccountKeys[limit.programIdIndex].toBase58(),'ComputeBudget111111111111111111111111111111');
      assert.equal(Buffer.from(limit.data).readUInt32LE(1),options.expectedUnits??PAYMENT_COMPUTE_UNITS);
      const keyIndex=transaction.message.staticAccountKeys.findIndex(k=>k.equals(key.publicKey));
      const publicKey=createPublicKey({key:Buffer.concat([Buffer.from('302a300506032b6570032100','hex'),key.publicKey.toBuffer()]),format:'der',type:'spki'});
      assert.ok(verify(null,transaction.message.serialize(),publicKey,transaction.signatures[keyIndex]));
      const memo=transaction.message.compiledInstructions.find(ix=>transaction.message.staticAccountKeys[ix.programIdIndex].toBase58()==='MemoSq4gqABAXKb96qnH8TysNcWxMyWCqXgDLGmfcHr');
      assert.equal(Buffer.from(memo.data).toString(),quote.extra.memo);
      transaction.sign([fee]); const txid=bs58.encode(transaction.signatures[0]);
      confirmed.set(txid,{meta:{err:null},transaction:[Buffer.from(transaction.serialize()).toString('base64'),'base64']});
      if(firstFailure){firstFailure=false;res.statusCode=503;res.end('{}');return;}
      if(firstPending){firstPending=false;res.statusCode=202;res.end('{}');return;}
      const output=[{type:'text',text:'<!doctype html><button onclick="this.textContent=Number(this.textContent)+1">0</button>'}];
      const job={job_id:quoteId,kind:'inference_call',capability_requirement:{kind:'inference_call',model_id:body.model},input:[{type:'json',value:{messages:body.messages}},{type:'json',value:{generation:{max_tokens:body.max_tokens,temperature:body.temperature}}}],price_micro_usdc:900};
      if(body.pricing==='model_tariff_v1') {
        const issued=Date.now();
        const tariff={schema_version:1,id:'fixture-tariff',revision:1,model:body.model,base_micro_usdc:500,output_cap_micro_usdc_per_million:1001,max_output_tokens:8192,valid_from_ms:issued-1000,valid_until_ms:issued+90000};
        const nativeQuote={tariff,requested_output_cap:body.max_tokens,price_micro_usdc:500+Number((BigInt(body.max_tokens)*1001n+999999n)/1000000n)};
        job.capability_requirement.inference_tariff=structuredClone(nativeQuote);
        job.input.push({type:'json',value:{inference_tariff:nativeQuote}});
        job.price_micro_usdc=nativeQuote.price_micro_usdc; job.issued_at_ms=issued;
      }
      if(options.badJob)options.badJob(job);
      const envelope=signature(buyer,'covenant.compute.job.v1\n',job,'payload','buyer');
      const receipt=signature(operator,'covenant.compute.receipt.v1\n',{job_id:quoteId,job_hash_hex:sha256(envelope.payload_json),result_hash_hex:sha256(stable(output)),price_micro_usdc:job.price_micro_usdc,status:'ok'},'receipt','operator');
      const result={choices:[{message:{role:'assistant',content:output[0].text}}],covenant:{envelope,receipt,output,output_json:stable(output),request_sha256:digest,quote_id:quoteId,job_id:quoteId}};
      if(options.badResult)options.badResult(result);
      res.setHeader('PAYMENT-RESPONSE',encode({success:true,network:NETWORK,payer:key.publicKey.toBase58(),transaction:txid}));res.end(JSON.stringify(result));
    } catch(error) { res.statusCode=500;res.end(JSON.stringify({error:error.message})); }
  });
  await new Promise(resolve=>server.listen(0,'127.0.0.1',resolve)); const base='http://127.0.0.1:'+server.address().port;
  class Wallet extends CovenantSolanaKeypairWalletProvider { async signTransaction(tx) {signatures++;return super.signTransaction(tx);} }
  const walletProvider=new Wallet({keypair:key.secretKey,rpcUrl:base+'/rpc',genesisHash:SOLANA_GENESIS});
  const config={gatewayUrl:base,rpcUrl:base+'/rpc',payTo:seller.publicKey.toBase58(),gatewayBuyer:buyer.publicKey.toBase58(),operators:[operator.publicKey.toBase58()],walletProvider,journalPath:join(home,'purchases.sqlite'),maxPaymentMicroUsdc:1000,maxTotalMicroUsdc:2000,allowLocalhost:true};
  return {config,walletProvider,walletKeypair:Array.from(key.secretKey),feeKeypair:Array.from(fee.secretKey),counts:()=>({requests,signatures,quotes,sends,payments}),cleanup:async()=>{await new Promise(resolve=>server.close(resolve));rmSync(home,{recursive:true,force:true});}};
}
