// Gate the live featured Covenant agent identity (4XtUr, mainnet) on the
// Covenant Oracle, then prove the gate closes on an invalid audit. Safe by
// construction: the asset is never moved, and it is left valid + transferable.
//
// Authority for both the asset (update authority) and the oracle verdict is
// DKxXr, Covenant's mainnet identity/data authority for this agent.

import { readFileSync } from 'node:fs';
import {
  Connection, Keypair, PublicKey, SystemProgram,
  Transaction, TransactionInstruction, sendAndConfirmTransaction,
} from '@solana/web3.js';
import { createUmi } from '@metaplex-foundation/umi-bundle-defaults';
import {
  createSignerFromKeypair, signerIdentity, generateSigner, publicKey,
} from '@metaplex-foundation/umi';
import { addPlugin, transfer, fetchAsset, CheckResult, mplCore } from '@metaplex-foundation/mpl-core';

const ASSET = process.argv[2];
const ORACLE_PROGRAM = new PublicKey('2PJFAtPsVzgLrmvj2Hwx7x1DuUXSjgW44qSR35MZshaD');
const KEYPAIR_PATH = `${process.env.HOME}/.config/solana/covenant-metaplex-authority.json`; // DKxXr
const envFile = `${process.env.HOME}/Projects/covenant/landing/.env.local`;
const RPC = process.env.RPC_URL ||
  readFileSync(envFile, 'utf8').match(/NEXT_PUBLIC_COVENANT_SOLANA_MAINNET_RPC_URL=(\S+)/)?.[1];
if (!RPC) throw new Error('no mainnet RPC');

const idl = JSON.parse(readFileSync(new URL('./idl/covenant_oracle_program.json', import.meta.url)));
const disc = (n) => Buffer.from(idl.instructions.find((i) => i.name === n).discriminator);

const secret = Uint8Array.from(JSON.parse(readFileSync(KEYPAIR_PATH)));
const dk = Keypair.fromSecretKey(secret);
const conn = new Connection(RPC, 'confirmed');
const umi = createUmi(RPC).use(mplCore());
umi.use(signerIdentity(createSignerFromKeypair(umi, umi.eddsa.createKeypairFromSecretKey(secret))));

const subject = new PublicKey(ASSET);
const pda = PublicKey.findProgramAddressSync([Buffer.from('oracle'), subject.toBuffer()], ORACLE_PROGRAM)[0];
const DKXXR = dk.publicKey.toBase58();
const RESULT = { 0: 'Approved', 1: 'Rejected', 2: 'Pass' };

const sendIx = (keys, data, commitment = 'confirmed') =>
  sendAndConfirmTransaction(conn,
    new Transaction().add(new TransactionInstruction({ programId: ORACLE_PROGRAM, keys, data })),
    [dk], { commitment });

async function verdict() {
  const a = await conn.getAccountInfo(pda, 'confirmed');
  return a ? RESULT[a.data[10]] : 'none';
}
async function waitFinalized() {
  for (let i = 0; i < 30; i++) {
    if (await conn.getAccountInfo(pda, 'finalized')) return;
    await new Promise((r) => setTimeout(r, 2000));
  }
  throw new Error('oracle account not finalized-visible after 60s');
}
const ownerOf = async () => (await fetchAsset(umi, publicKey(ASSET))).owner;
const hasOracle = async () =>
  (await fetchAsset(umi, publicKey(ASSET))).oracles?.some((o) => o.baseAddress === pda.toBase58());

console.log(`\nGate live identity ${ASSET} on mainnet`);
console.log(`oracle program ${ORACLE_PROGRAM.toBase58()}`);
console.log(`oracle pda     ${pda.toBase58()}`);
console.log(`authority      ${DKXXR}\n`);

if (!(await conn.getAccountInfo(pda))) {
  const sig = await sendIx([
    { pubkey: pda, isSigner: false, isWritable: true },
    { pubkey: dk.publicKey, isSigner: true, isWritable: true },
    { pubkey: SystemProgram.programId, isSigner: false, isWritable: false },
  ], Buffer.concat([disc('init_oracle'), subject.toBuffer()]), 'finalized');
  console.log(`init_oracle    ${sig}  verdict=${await verdict()}`);
} else {
  console.log(`init_oracle    already exists  verdict=${await verdict()}`);
}

if (!(await hasOracle())) {
  const { signature } = await addPlugin(umi, {
    asset: publicKey(ASSET),
    plugin: {
      type: 'Oracle', baseAddress: publicKey(pda.toBase58()),
      resultsOffset: { type: 'Anchor' }, lifecycleChecks: { transfer: [CheckResult.CAN_REJECT] },
    },
  }).sendAndConfirm(umi, { confirm: { commitment: 'confirmed' } });
  console.log(`addPlugin      ${Buffer.from(signature).toString('base64').slice(0, 12)}.. (oracle now on 4XtUr)`);
} else {
  console.log('addPlugin      oracle already on 4XtUr');
}

// Prove the gate closes. set invalid (finalized so every RPC node sees it),
// attempt a transfer to a throwaway, confirm MPL Core vetoes and 4XtUr stays put.
await waitFinalized();
console.log('\nset_validation(false) [finalized]');
await sendIx([
  { pubkey: pda, isSigner: false, isWritable: true },
  { pubkey: dk.publicKey, isSigner: true, isWritable: false },
], Buffer.concat([disc('set_validation'), Buffer.from([0])]), 'finalized');
console.log(`  verdict=${await verdict()}`);

let rejected = false;
try {
  await transfer(umi, { asset: await fetchAsset(umi, publicKey(ASSET)), newOwner: generateSigner(umi).publicKey })
    .sendAndConfirm(umi, { confirm: { commitment: 'confirmed' } });
} catch (e) {
  const ctx = [e.message, e.cause?.message ?? e.cause, JSON.stringify(e.transactionLogs ?? e.logs ?? '')].join(' ');
  rejected = /0x9|Invalid Authority|custom program error|lifecycle|Reject/i.test(ctx);
  console.log(`  transfer rejected: ${String(e.message || e).split('\n')[0].slice(0, 80)}`);
}
const ownerAfterReject = await ownerOf();
console.log(`  rejected=${rejected}  owner==DKxXr (unmoved)=${ownerAfterReject === DKXXR}`);

// Restore to valid and HARD-VERIFY 4XtUr is left transferable + owned by DKxXr.
console.log('\nset_validation(true) [finalized] — restore');
await sendIx([
  { pubkey: pda, isSigner: false, isWritable: true },
  { pubkey: dk.publicKey, isSigner: true, isWritable: false },
], Buffer.concat([disc('set_validation'), Buffer.from([1])]), 'finalized');

const finalVerdict = await verdict();
const finalOwner = await ownerOf();
const oraclePresent = await hasOracle();
console.log(`  verdict=${finalVerdict}  owner=${finalOwner}  oracleOn4XtUr=${oraclePresent}`);

const safe = rejected && ownerAfterReject === DKXXR && finalVerdict === 'Pass'
  && finalOwner === DKXXR && oraclePresent;
console.log(`\n${safe ? 'MAINNET GATING LIVE' : 'CHECK FAILED'} — 4XtUr is Oracle-gated, currently valid + transferable.`);
console.log(`metaplex: https://www.metaplex.com/agents/${ASSET}`);
console.log(`solscan oracle: https://solscan.io/account/${pda.toBase58()}`);
process.exit(safe ? 0 : 1);
