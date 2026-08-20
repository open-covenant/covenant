// Gate a live featured Covenant agent identity (the MPL Core asset passed as
// argv[2], on mainnet) on the Covenant Oracle, then prove the gate closes on an
// invalid audit. The veto is proven by SIMULATION only, so the real asset is
// never broadcast-transferred and is left valid + transferable.
//
// Authority for both the asset (update authority) and the oracle verdict is
// DKxXr, Covenant's mainnet identity/data authority for these agents.
//
// usage: node wire-gate.mjs <asset-pubkey>

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
  addPlugin, transfer, fetchAsset, fetchAssetV1, CheckResult, mplCore,
} from '@metaplex-foundation/mpl-core';

const ORACLE_PROGRAM = new PublicKey('2PJFAtPsVzgLrmvj2Hwx7x1DuUXSjgW44qSR35MZshaD');
const KEYPAIR_PATH = `${process.env.HOME}/.config/solana/covenant-metaplex-authority.json`; // DKxXr
const envFile = `${process.env.HOME}/Projects/covenant/landing/.env.local`;

// The RPC carries an api-key query param; keep it out of any thrown error.
const redact = (s) => String(s).replace(/api-key=[^&\s"']+/gi, 'api-key=REDACTED');

const ASSET = process.argv[2];
let subject;
try {
  if (!ASSET) throw new Error('missing asset');
  subject = new PublicKey(ASSET);
} catch {
  console.error('usage: node wire-gate.mjs <asset-pubkey>   (base58 MPL Core asset address)');
  process.exit(2);
}
const tag = ASSET.slice(0, 5);

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

// OracleState: disc(8) + validation V1(5) then the authority pubkey at 13..45.
const readAuthority = (data) => {
  if (data.length < 45) throw new Error('oracle account too small to hold an authority');
  return new PublicKey(data.subarray(13, 45)).toBase58();
};

async function waitFinalized() {
  for (let i = 0; i < 30; i++) {
    if (await conn.getAccountInfo(pda, 'finalized')) return;
    await new Promise((r) => setTimeout(r, 2000));
  }
  throw new Error('oracle account not finalized-visible after 60s');
}

// addPlugin lands at 'confirmed'; the gate only exists once the Oracle plugin is
// rooted on the asset, so block on a finalized read before proving the veto.
async function waitOraclePlugin() {
  for (let i = 0; i < 30; i++) {
    const a = await fetchAssetV1(umi, publicKey(ASSET), { commitment: 'finalized' });
    if (a.oracles?.some((o) => o.baseAddress === pda.toBase58())) return;
    await new Promise((r) => setTimeout(r, 2000));
  }
  throw new Error('oracle plugin not finalized-visible on asset after 60s');
}

const ownerOf = async () => (await fetchAsset(umi, publicKey(ASSET))).owner;
const hasOracle = async () =>
  (await fetchAsset(umi, publicKey(ASSET))).oracles?.some((o) => o.baseAddress === pda.toBase58());

const toWeb3Ix = (ix) => new TransactionInstruction({
  programId: new PublicKey(ix.programId),
  keys: ix.keys.map((k) => ({ pubkey: new PublicKey(k.pubkey), isSigner: k.isSigner, isWritable: k.isWritable })),
  data: Buffer.from(ix.data),
});

let invalidated = false;
let restoring = false;

async function restoreValid(attempts = 3) {
  for (let i = 1; i <= attempts; i++) {
    try {
      await sendIx([
        { pubkey: pda, isSigner: false, isWritable: true },
        { pubkey: dk.publicKey, isSigner: true, isWritable: false },
      ], Buffer.concat([disc('set_validation'), Buffer.from([1])]), 'finalized');
      invalidated = false;
      return;
    } catch (e) {
      console.error(`  restore attempt ${i}/${attempts} failed: ${redact(String(e.message || e)).split('\n')[0].slice(0, 100)}`);
      if (i === attempts) throw e;
      await new Promise((r) => setTimeout(r, 3000));
    }
  }
}

// Leaving the asset Rejected would freeze a live identity, so any abnormal exit
// has to put the verdict back to valid first.
async function emergencyRestore(reason) {
  if (!invalidated || restoring) return;
  restoring = true;
  console.error(`\n${reason}: asset left Rejected, restoring to valid before exit`);
  try {
    await restoreValid();
    console.error('restored to valid');
  } catch (e) {
    console.error(`RESTORE FAILED, asset still Rejected: ${redact(String(e.message || e))}`);
  } finally {
    restoring = false;
  }
}
process.on('SIGINT', () => emergencyRestore('SIGINT').finally(() => process.exit(130)));
process.on('SIGTERM', () => emergencyRestore('SIGTERM').finally(() => process.exit(143)));

async function main() {
  console.log(`\nGate live identity ${ASSET} on mainnet`);
  console.log(`oracle program ${ORACLE_PROGRAM.toBase58()}`);
  console.log(`oracle pda     ${pda.toBase58()}`);
  console.log(`authority      ${DKXXR}\n`);

  const existing = await conn.getAccountInfo(pda, 'confirmed');
  if (!existing) {
    const sig = await sendIx([
      { pubkey: pda, isSigner: false, isWritable: true },
      { pubkey: dk.publicKey, isSigner: true, isWritable: true },
      { pubkey: SystemProgram.programId, isSigner: false, isWritable: false },
    ], Buffer.concat([disc('init_oracle'), subject.toBuffer()]), 'finalized');
    console.log(`init_oracle    ${sig}  verdict=${await verdict()}`);
  } else {
    // A pre-existing PDA could be squatted; only trust an oracle whose stored
    // authority is our signing key before wiring the asset to it.
    const onChain = readAuthority(existing.data);
    if (onChain !== DKXXR)
      throw new Error(`oracle PDA ${pda.toBase58()} authority is ${onChain}, expected ${DKXXR}; refusing to wire a foreign oracle`);
    console.log(`init_oracle    already exists  verdict=${await verdict()}  authority ok`);
  }

  if (!(await hasOracle())) {
    const { signature } = await addPlugin(umi, {
      asset: publicKey(ASSET),
      plugin: {
        type: 'Oracle', baseAddress: publicKey(pda.toBase58()),
        resultsOffset: { type: 'Anchor' }, lifecycleChecks: { transfer: [CheckResult.CAN_REJECT] },
      },
    }).sendAndConfirm(umi, { confirm: { commitment: 'confirmed' } });
    console.log(`addPlugin      ${Buffer.from(signature).toString('base64').slice(0, 12)}.. (oracle now on ${tag})`);
  } else {
    console.log(`addPlugin      oracle already on ${tag}`);
  }

  // Prove the gate closes. Set invalid (finalized so every RPC node sees it),
  // require the verdict really is Rejected, then prove MPL Core vetoes a transfer
  // by simulation only so the asset is never moved. Restore to valid no matter what.
  await waitFinalized();
  await waitOraclePlugin();

  console.log('\nset_validation(false) [finalized]');
  await sendIx([
    { pubkey: pda, isSigner: false, isWritable: true },
    { pubkey: dk.publicKey, isSigner: true, isWritable: false },
  ], Buffer.concat([disc('set_validation'), Buffer.from([0])]), 'finalized');
  invalidated = true;

  let rejected = false;
  let ownerAfterReject;
  try {
    const v = await verdict();
    console.log(`  verdict=${v}`);
    if (v !== 'Rejected')
      throw new Error(`expected verdict Rejected after set_validation(false), got ${v}; aborting before the proof`);

    const builder = transfer(umi, {
      asset: await fetchAsset(umi, publicKey(ASSET)),
      newOwner: generateSigner(umi).publicKey,
    });
    const simTx = new Transaction().add(...builder.getInstructions().map(toWeb3Ix));
    simTx.feePayer = dk.publicKey;
    const sim = await conn.simulateTransaction(simTx, [dk]);
    const logs = sim.value.logs ?? [];
    // MPL Core vetoes a Rejected oracle with custom error 0x9 (decoded "Invalid
    // Authority"); match both the raw log form and the structured err so a logless
    // RPC response still counts as a reject.
    const ctx = `${JSON.stringify(sim.value.err ?? '')} ${logs.join('\n')}`;
    rejected = sim.value.err != null &&
      /0x9|"Custom":\s*9\b|custom program error|lifecycle|invalid authority|reject/i.test(ctx);
    const line = logs.filter((l) => /0x9|reject|fail/i.test(l)).slice(-1)[0];
    console.log(`  transfer SIMULATED (never broadcast)  rejected=${rejected}`);
    if (line) console.log(`  reject log: ${redact(line).slice(0, 100)}`);

    ownerAfterReject = await ownerOf();
    console.log(`  owner==DKxXr (unmoved)=${ownerAfterReject === DKXXR}`);
  } finally {
    console.log('\nset_validation(true) [finalized] restore');
    await restoreValid();
  }

  const finalVerdict = await verdict();
  const finalOwner = await ownerOf();
  const oraclePresent = await hasOracle();
  console.log(`  verdict=${finalVerdict}  owner=${finalOwner}  oracleOn${tag}=${oraclePresent}`);

  const safe = rejected && ownerAfterReject === DKXXR && finalVerdict === 'Pass'
    && finalOwner === DKXXR && oraclePresent;
  console.log(`\n${safe ? 'MAINNET GATING LIVE' : 'CHECK FAILED'}: ${tag} is Oracle-gated, currently valid + transferable.`);
  console.log(`metaplex: https://www.metaplex.com/agents/${ASSET}`);
  console.log(`solscan oracle: https://solscan.io/account/${pda.toBase58()}`);
  process.exit(safe ? 0 : 1);
}

main().catch(async (e) => {
  console.error(redact(String(e?.stack || e?.message || e)));
  await emergencyRestore('error');
  process.exit(1);
});
