// Devnet proof that the Covenant Oracle gates an MPL Core agent asset's
// transfer on a live audit verdict.
//
//   1. init_oracle(subject = asset)  -> oracle PDA, audit valid by default
//   2. create asset + Oracle plugin  -> transfer gated on the PDA (offset Anchor)
//   3. set_validation(false)         -> audit invalid / out of policy
//   4. transfer                      -> REJECTED by MPL Core, owner unchanged
//   5. set_validation(true)          -> audit restored
//   6. transfer                      -> SUCCEEDS, owner moves
//
// The rule is enforced by MPL Core itself; Covenant only flips the verdict.

import { readFileSync } from 'node:fs';
import {
  Connection, Keypair, PublicKey, SystemProgram,
  Transaction, TransactionInstruction, sendAndConfirmTransaction,
} from '@solana/web3.js';
import { createUmi } from '@metaplex-foundation/umi-bundle-defaults';
import {
  createSignerFromKeypair, signerIdentity, generateSigner, publicKey,
} from '@metaplex-foundation/umi';
import {
  create, transfer, fetchAsset, CheckResult, mplCore,
} from '@metaplex-foundation/mpl-core';

const RPC = process.env.RPC_URL || 'https://api.devnet.solana.com';
const KEYPAIR_PATH = process.env.KEYPAIR || `${process.env.HOME}/.config/solana/id.json`;
const ORACLE_PROGRAM = new PublicKey('2PJFAtPsVzgLrmvj2Hwx7x1DuUXSjgW44qSR35MZshaD');

const idl = JSON.parse(readFileSync(new URL('./idl/covenant_oracle_program.json', import.meta.url)));
const disc = (name) => Buffer.from(idl.instructions.find((i) => i.name === name).discriminator);

const secret = Uint8Array.from(JSON.parse(readFileSync(KEYPAIR_PATH)));
const payer = Keypair.fromSecretKey(secret);
const conn = new Connection(RPC, 'confirmed');

const umi = createUmi(RPC).use(mplCore());
umi.use(signerIdentity(createSignerFromKeypair(umi, umi.eddsa.createKeypairFromSecretKey(secret))));

const oraclePda = (subject) =>
  PublicKey.findProgramAddressSync([Buffer.from('oracle'), subject.toBuffer()], ORACLE_PROGRAM)[0];

const sendIx = (keys, data, commitment = 'confirmed') =>
  sendAndConfirmTransaction(conn, new Transaction().add(
    new TransactionInstruction({ programId: ORACLE_PROGRAM, keys, data }),
  ), [payer], { commitment });

const initOracle = (subject) =>
  sendIx(
    [
      { pubkey: oraclePda(subject), isSigner: false, isWritable: true },
      { pubkey: payer.publicKey, isSigner: true, isWritable: true },
      { pubkey: SystemProgram.programId, isSigner: false, isWritable: false },
    ],
    Buffer.concat([disc('init_oracle'), subject.toBuffer()]),
  );

// finalized: the verdict must be rooted on every devnet RPC node before the
// transfer's preflight reads it, else a lagging load-balanced node sees the
// stale value and the gate appears to flap.
const setValidation = (subject, valid) =>
  sendIx(
    [
      { pubkey: oraclePda(subject), isSigner: false, isWritable: true },
      { pubkey: payer.publicKey, isSigner: true, isWritable: false },
    ],
    Buffer.concat([disc('set_validation'), Buffer.from([valid ? 1 : 0])]),
    'finalized',
  );

// transfer verdict byte lives at offset 8 (anchor disc) + 1 (V1 tag) + 1 (create)
const RESULT = { 0: 'Approved', 1: 'Rejected', 2: 'Pass' };
async function readTransferVerdict(subject) {
  const acct = await conn.getAccountInfo(oraclePda(subject));
  return RESULT[acct.data[10]] ?? `raw(${acct.data[10]})`;
}

async function ownerOf(asset) {
  return (await fetchAsset(umi, asset)).owner;
}

async function tryTransfer(asset, newOwner) {
  const fetched = await fetchAsset(umi, asset);
  await transfer(umi, { asset: fetched, newOwner }).sendAndConfirm(umi);
}

const short = (s) => `${String(s).slice(0, 4)}..${String(s).slice(-4)}`;
let failed = false;
const check = (label, ok) => {
  console.log(`  ${ok ? 'PASS' : 'FAIL'}  ${label}`);
  if (!ok) failed = true;
};

async function main() {
  console.log(`\nCovenant Oracle gating demo  (${RPC})`);
  console.log(`oracle program  ${ORACLE_PROGRAM.toBase58()}`);
  console.log(`authority       ${payer.publicKey.toBase58()}\n`);

  const asset = generateSigner(umi);
  const subject = new PublicKey(asset.publicKey);
  const pda = oraclePda(subject);
  const recipient = generateSigner(umi).publicKey;
  console.log(`asset           ${asset.publicKey}`);
  console.log(`oracle pda      ${pda.toBase58()}`);
  console.log(`recipient       ${recipient}\n`);

  console.log('1. init_oracle (audit valid by default)');
  console.log(`   ${short(await initOracle(subject))}  verdict=${await readTransferVerdict(subject)}`);

  console.log('2. create agent asset, transfer gated on the oracle');
  await create(umi, {
    asset,
    name: 'Covenant Oracle Demo Agent',
    uri: 'https://opencovenant.org/agents/oracle-demo.json',
    plugins: [{
      type: 'Oracle',
      baseAddress: publicKey(pda.toBase58()),
      resultsOffset: { type: 'Anchor' },
      lifecycleChecks: { transfer: [CheckResult.CAN_REJECT] },
    }],
  }).sendAndConfirm(umi);
  check('asset minted, owner = authority', (await ownerOf(asset.publicKey)) === umi.identity.publicKey);

  console.log('\n3. set_validation(false)  -> audit invalid / out of policy');
  console.log(`   ${short(await setValidation(subject, false))}  verdict=${await readTransferVerdict(subject)}`);

  console.log('4. transfer (expect REJECTED by MPL Core)');
  let rejected = false;
  try {
    await tryTransfer(asset.publicKey, recipient);
  } catch (e) {
    rejected = true;
    console.log(`   rejected: ${String(e.message || e).split('\n')[0].slice(0, 120)}`);
  }
  check('transfer threw while audit invalid', rejected);
  check('owner unchanged after rejected transfer', (await ownerOf(asset.publicKey)) === umi.identity.publicKey);

  console.log('\n5. set_validation(true)  -> audit restored');
  console.log(`   ${short(await setValidation(subject, true))}  verdict=${await readTransferVerdict(subject)}`);

  console.log('6. transfer (expect SUCCESS)');
  let allowed = true;
  try {
    await tryTransfer(asset.publicKey, recipient);
  } catch (e) {
    allowed = false;
    console.log(`   unexpected failure: ${String(e.message || e).split('\n')[0].slice(0, 120)}`);
  }
  check('transfer succeeded while audit valid', allowed);
  check('owner moved to recipient', (await ownerOf(asset.publicKey)) === recipient);

  console.log(`\n${failed ? 'DEMO FAILED' : 'DEMO PASSED'} — gate closes on invalid audit, opens on valid.\n`);
  console.log(`solscan asset:  https://solscan.io/token/${asset.publicKey}?cluster=devnet`);
  console.log(`solscan oracle: https://solscan.io/account/${pda.toBase58()}?cluster=devnet\n`);
  process.exit(failed ? 1 : 0);
}

main().catch((e) => { console.error(e); process.exit(1); });
