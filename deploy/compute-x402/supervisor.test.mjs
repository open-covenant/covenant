import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import {spawnSync} from 'node:child_process';
import crypto from 'node:crypto';

const BASE58='123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz';
/** A real Ed25519 keypair from a repeated seed byte: base64 of its 64-byte array, and its address. */
function keypair(byte){
 const {privateKey,publicKey}=crypto.generateKeyPairSync('ed25519');
 const seed=Buffer.alloc(32,byte);
 const secret=crypto.createPrivateKey({key:Buffer.concat([Buffer.from('302e020100300506032b657004220420','hex'),seed]),format:'der',type:'pkcs8'});
 const pub=crypto.createPublicKey(secret).export({format:'der',type:'spki'}).subarray(-32);
 let n=[...pub].reduce((v,b)=>v*256n+BigInt(b),0n),address='';
 while(n>0n){address=BASE58[Number(n%58n)]+address;n/=58n;}
 for(const b of pub){if(b!==0)break;address='1'+address;}
 void privateKey;void publicKey;
 return {b64:Buffer.from(JSON.stringify([...seed,...pub])).toString('base64'),address,pub};
}
const FEE_PAYER=keypair(1);

function boot(t,{tariffRuntime=false,env={}}={}){
 const dir=fs.mkdtempSync(path.join(os.tmpdir(),'covenant-supervisor-compat-'));
 t.after(()=>fs.rmSync(dir,{recursive:true,force:true}));
 const source=path.join(dir,'deploy','compute-x402'),data=path.join(dir,'data'),calls=path.join(dir,'calls.jsonl');
 fs.mkdirSync(source,{recursive:true});
 fs.copyFileSync(new URL('./supervisor.mjs',import.meta.url),path.join(source,'supervisor.mjs'));
 fs.writeFileSync(path.join(dir,'deploy','compute-canary-watchdog.mjs'),'export function startWatchdog(){return ()=>{};}');
 // Exercise the actual supervisor with the legacy export surface. Filesystem
 // and identity handling are real; child processes and readiness have no network.
 fs.writeFileSync(path.join(source,'runtime.mjs'),`
  import fs from 'node:fs';
  export {privateDirectory,seed,latch,missing,clearStartingLatch,explicitBoolean,operatorPubkey,startInferenceOperator${tariffRuntime?',materializeTariffs':''}} from ${JSON.stringify(new URL('./runtime.mjs',import.meta.url).href)};
  export class ProcessGroup {
   start(name,command,args,env,optional=false){fs.appendFileSync(process.env.TEST_CALLS,JSON.stringify({name,env,optional})+'\\n');}
   stop(code){process.exitCode=code;}
  }
  export async function waitFor(){}
 `);
 const result=spawnSync(process.execPath,[path.join(source,'supervisor.mjs')],{encoding:'utf8',timeout:5000,env:{
  TEST_CALLS:calls,COVENANT_X402_DATA:data,COVENANT_X402_OPERATOR_ENABLED:'false',COVENANT_GATEWAY_ADMISSIONS_ENABLED:'false',
  COVENANT_GATEWAY_IDENTITY_B64:Buffer.alloc(32,7).toString('base64'),COVENANT_GATEWAY_FEE_PAYER_KEY_B64:FEE_PAYER.b64,COVENANT_GATEWAY_FEE_PAYER:FEE_PAYER.address,
  COVENANT_GATEWAY_BRIDGE_SECRET:'test-secret-'.repeat(4),...env,
 }});
 assert.equal(result.error,undefined);
 assert.equal(result.status,0,result.stderr);
 assert.equal(result.stderr,'');
 const workers=fs.readFileSync(calls,'utf8').trim().split('\n').map(line=>JSON.parse(line));
 assert.deepEqual(workers.map(worker=>worker.name),['payment verifier','inference gateway']);
 return {data,workers,disabled:path.join(data,'gateway','admissions.disabled')};
}

test('observer overlay boots with the legacy runtime export surface and no tariffs',t=>{
 const {disabled}=boot(t);
 assert.equal(fs.existsSync(disabled),false);
});

test('observer overlay refuses every tariff setting but keeps essential recovery workers',t=>{
 for(const key of ['COVENANT_GATEWAY_TARIFFS_JSON','COVENANT_GATEWAY_TARIFFS_FILE','COVENANT_COMPUTE_NODE_TARIFFS_FILE']){
  const {disabled,workers}=boot(t,{env:{COVENANT_X402_OPERATOR_ENABLED:'true',[key]:''}});
  assert.equal(fs.readFileSync(disabled,'utf8'),'Tariffs require the full pricing runtime image; payment recovery only\n');
  for(const {env} of workers)for(const name of ['COVENANT_GATEWAY_TARIFFS_JSON','COVENANT_GATEWAY_TARIFFS_FILE','COVENANT_COMPUTE_NODE_TARIFFS_FILE'])assert.equal(Object.hasOwn(env,name),false);
 }
});

test('full runtime materializes configured tariffs for both financial workers',t=>{
 const tariff={schema_version:1,id:'model',revision:1,model:'model',base_micro_usdc:100,output_cap_micro_usdc_per_million:1000,max_output_tokens:2048,valid_from_ms:1,valid_until_ms:9999999999999};
 const rows=[{native:tariff,retail:{...tariff,base_micro_usdc:200}}];
 const {disabled,workers}=boot(t,{tariffRuntime:true,env:{COVENANT_GATEWAY_TARIFFS_JSON:JSON.stringify(rows)}});
 assert.equal(fs.existsSync(disabled),false);
 for(const {env} of workers){
  assert.deepEqual(JSON.parse(fs.readFileSync(env.COVENANT_GATEWAY_TARIFFS_FILE)),rows);
  assert.deepEqual(JSON.parse(fs.readFileSync(env.COVENANT_COMPUTE_NODE_TARIFFS_FILE)),[tariff]);
  assert.equal(Object.hasOwn(env,'COVENANT_GATEWAY_TARIFFS_JSON'),false);
 }
});

test('Base facilitator credentials and RPC reach only the payment verifier',t=>{
 const secrets={CDP_API_KEY_ID:'cdp-id',CDP_API_KEY_SECRET:'cdp-secret',COVENANT_GATEWAY_BASE_RPC:'https://base.example/key',COVENANT_GATEWAY_BASE_FACILITATOR_URL:'https://facilitator.example'};
 const {workers}=boot(t,{env:{...secrets,COVENANT_GATEWAY_BASE_PAY_TO:'0x5fA1d0C0bfFE257a20027C523093F941834f5D66'}});
 const [verifier,gateway]=workers;
 for(const [key,value] of Object.entries(secrets)){assert.equal(verifier.env[key],value);assert.equal(Object.hasOwn(gateway.env,key),false);}
 assert.equal(gateway.env.COVENANT_GATEWAY_BASE_PAY_TO,'0x5fA1d0C0bfFE257a20027C523093F941834f5D66');
});

test('a fee payer key that does not match its address is refused before anything is written',t=>{
 const dir=fs.mkdtempSync(path.join(os.tmpdir(),'covenant-supervisor-fee-payer-'));
 t.after(()=>fs.rmSync(dir,{recursive:true,force:true}));
 const data=path.join(dir,'data');
 const result=spawnSync(process.execPath,[new URL('./supervisor.mjs',import.meta.url).pathname],{encoding:'utf8',timeout:5000,env:{
  COVENANT_X402_DATA:data,COVENANT_X402_OPERATOR_ENABLED:'false',COVENANT_GATEWAY_ADMISSIONS_ENABLED:'false',
  COVENANT_GATEWAY_IDENTITY_B64:Buffer.alloc(32,7).toString('base64'),
  COVENANT_GATEWAY_FEE_PAYER_KEY_B64:Buffer.from(JSON.stringify([...Array(32).fill(1),...Array(32).fill(2)])).toString('base64'),
  COVENANT_GATEWAY_FEE_PAYER:'1'.repeat(32),COVENANT_GATEWAY_BRIDGE_SECRET:'test-secret-'.repeat(4),
 }});
 assert.notEqual(result.status,0);
 // Startup stopped at the fee payer: the identity seeded just before it exists,
 // and neither a fee payer file nor the bridge secret seeded after it does.
 const written=fs.readdirSync(path.join(data,'gateway'));
 assert.ok(written.includes('identity.key'));
 assert.deepEqual(written.filter(name=>name.startsWith('fee-payer')||name==='bridge-secret'),[]);
});


test('paid jobs seed their merchant key for the verifier alone and their own buyer identity',t=>{
 const merchant=keypair(3);
 const jobsIdentity=Buffer.alloc(32,9).toString('base64');
 const {data,workers}=boot(t,{env:{COVENANT_GATEWAY_JOBS_PAY_TO:merchant.address,
  COVENANT_GATEWAY_JOBS_MERCHANT_KEY_B64:merchant.b64,COVENANT_GATEWAY_JOBS_IDENTITY_B64:jobsIdentity,
  COVENANT_GATEWAY_JOBS_RESEARCH_PRICE_MICRO_USDC:'5000000'}});
 const [verifier,gateway]=workers;
 const merchantFile=path.join(data,'verifier',`jobs-merchant-${merchant.address}.json`);
 assert.equal(verifier.env.COVENANT_GATEWAY_JOBS_MERCHANT_KEY_FILE,merchantFile);
 assert.equal(fs.readFileSync(merchantFile).toString('base64'),merchant.b64);
 assert.equal(fs.readdirSync(path.join(data,'gateway')).some(name=>name.startsWith('jobs-merchant')),false);
 assert.equal(Object.hasOwn(gateway.env,'COVENANT_GATEWAY_JOBS_MERCHANT_KEY_FILE'),false);
 assert.deepEqual(fs.readFileSync(path.join(data,'gateway','jobs-identity.key')),Buffer.alloc(32,9));
 for(const {env} of workers){
  assert.equal(Object.hasOwn(env,'COVENANT_GATEWAY_JOBS_MERCHANT_KEY_B64'),false);
  assert.equal(Object.hasOwn(env,'COVENANT_GATEWAY_JOBS_IDENTITY_B64'),false);
  assert.equal(env.COVENANT_GATEWAY_JOBS_RESEARCH_PRICE_MICRO_USDC,'5000000');
 }
});

test('a jobs merchant key that is not the one for its address is refused before it is written',t=>{
 const other=keypair(4),merchant=keypair(3);
 // Another wallet's key, and a key whose public half does not match its secret half.
 const forged=Buffer.from(JSON.stringify([...Buffer.alloc(32,3),...other.pub])).toString('base64');
 for(const key of [other.b64,forged]){
  const dir=fs.mkdtempSync(path.join(os.tmpdir(),'covenant-supervisor-jobs-'));
  t.after(()=>fs.rmSync(dir,{recursive:true,force:true}));
  const data=path.join(dir,'data');
  const result=spawnSync(process.execPath,[new URL('./supervisor.mjs',import.meta.url).pathname],{encoding:'utf8',timeout:5000,env:{
   COVENANT_X402_DATA:data,COVENANT_X402_OPERATOR_ENABLED:'false',COVENANT_GATEWAY_ADMISSIONS_ENABLED:'false',
   COVENANT_GATEWAY_IDENTITY_B64:Buffer.alloc(32,7).toString('base64'),
   COVENANT_GATEWAY_FEE_PAYER_KEY_B64:FEE_PAYER.b64,COVENANT_GATEWAY_FEE_PAYER:FEE_PAYER.address,COVENANT_GATEWAY_BRIDGE_SECRET:'test-secret-'.repeat(4),
   COVENANT_GATEWAY_JOBS_PAY_TO:key===forged?other.address:merchant.address,COVENANT_GATEWAY_JOBS_IDENTITY_B64:Buffer.alloc(32,9).toString('base64'),
   COVENANT_GATEWAY_JOBS_MERCHANT_KEY_B64:key,
  }});
  assert.notEqual(result.status,0);
  assert.equal(fs.existsSync(path.join(data,'verifier')),false);
  assert.deepEqual(fs.readdirSync(path.join(data,'gateway')).filter(name=>name.startsWith('jobs-')),[]);
 }
});
