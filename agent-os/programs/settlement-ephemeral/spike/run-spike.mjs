// Live devnet spike for the Covenant settlement ER build.
//
// Uses the already-initialized cov9UDyp devnet deployment (config + a credit
// account with a positive balance), so no token/mint setup is needed. Flow:
//   delegate_credits (L1) -> N x consume_credits (ER) -> undelegate_credits (ER)
//   -> read balance on L1 and assert exact reconciliation.
//
// Instructions are encoded by hand (anchor global discriminator = sha256("global:<ix>")[:8])
// with the exact account order the #[delegate]/#[commit] macros generate. ER txs go
// straight to the pinned validator endpoint; L1 txs to devnet.
//
// Env: PAYER (keypair json, default id.json), N (default 50), AMOUNT (default 1),
//      ER (validator RPC, default EU), VALIDATOR (pinned pubkey, default EU).

import fs from "node:fs";
import os from "node:os";
import crypto from "node:crypto";
import {
  Connection, Keypair, PublicKey, SystemProgram,
  Transaction, TransactionInstruction, sendAndConfirmTransaction,
} from "@solana/web3.js";
import { DELEGATION_PROGRAM_ID, MAGIC_PROGRAM_ID, MAGIC_CONTEXT_ID,
  delegateBufferPdaFromDelegatedAccountAndOwnerProgram,
  delegationRecordPdaFromDelegatedAccount,
  delegationMetadataPdaFromDelegatedAccount } from "@magicblock-labs/ephemeral-rollups-sdk";

const PROG = new PublicKey("cov9UDypG7nsryxdgMcKhKU2spRVWLVjxT2iTv6do5Y");
const pk = (x) => (x instanceof PublicKey ? x : new PublicKey(x));
const env = (k, d) => process.env[k] ?? d;
const N = Number(env("N", "50"));
const AMOUNT = BigInt(env("AMOUNT", "1"));
const ER_URL = env("ER", "https://devnet-eu.magicblock.app");
const VALIDATOR = new PublicKey(env("VALIDATOR", "MEUGGrYPxKk17hCr7wpT6s8dtNokZj5U2L57vjYMS8e"));

const payer = Keypair.fromSecretKey(new Uint8Array(JSON.parse(
  fs.readFileSync(env("PAYER", `${os.homedir()}/.config/solana/id.json`), "utf8"))));
const owner = payer.publicKey;

const l1 = new Connection("https://api.devnet.solana.com", "confirmed");
const er = new Connection(ER_URL, "confirmed");

const pda = (seeds) => PublicKey.findProgramAddressSync(seeds, PROG)[0];
const config = pda([Buffer.from("config")]);
const credits = pda([Buffer.from("credits"), owner.toBuffer()]);
const disc = (name) => crypto.createHash("sha256").update(`global:${name}`).digest().subarray(0, 8);
const m = (pubkey, isSigner, isWritable) => ({ pubkey: pk(pubkey), isSigner, isWritable });

async function balanceL1() {
  const ai = await l1.getAccountInfo(credits);
  return ai ? ai.data.readBigUInt64LE(40) : null;
}
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

async function send(conn, ix, label) {
  const tx = new Transaction().add(ix);
  const sig = await sendAndConfirmTransaction(conn, tx, [payer], { commitment: "confirmed", skipPreflight: false });
  console.log(`  ${label}: ${sig}`);
  return sig;
}

async function main() {
  console.log(`program ${PROG} | credits ${credits}`);
  const before = await balanceL1();
  console.log(`starting balance (L1): ${before}`);
  if (before === null) throw new Error("credit account missing");
  if (before < AMOUNT * BigInt(N)) throw new Error(`need >= ${AMOUNT * BigInt(N)} credits`);

  // 1) delegate on L1. Account order matches the #[delegate] macro expansion.
  console.log(`\n[1] delegate_credits (L1), pinning validator ${VALIDATOR}`);
  const delIx = new TransactionInstruction({
    programId: PROG,
    keys: [
      m(owner, true, true),                                                      // payer
      m(delegateBufferPdaFromDelegatedAccountAndOwnerProgram(credits, PROG), false, true), // buffer_pda
      m(delegationRecordPdaFromDelegatedAccount(credits), false, true),          // delegation_record
      m(delegationMetadataPdaFromDelegatedAccount(credits), false, true),        // delegation_metadata
      m(credits, false, true),                                                   // pda (credits)
      m(PROG, false, false),                                                     // owner_program
      m(DELEGATION_PROGRAM_ID, false, false),                                    // delegation_program
      m(SystemProgram.programId, false, false),                                  // system_program
      m(VALIDATOR, false, false),                                                // remaining[0]: pinned validator
    ],
    data: disc("delegate_credits"),
  });
  await send(l1, delIx, "delegate");
  console.log("  waiting for the ER to pick up the delegated account...");
  await sleep(6000);

  // 2) N x consume_credits in the ER.
  console.log(`\n[2] ${N} x consume_credits in the ER (${ER_URL})`);
  const receipt = Buffer.alloc(32);
  const consumeData = (amount) => Buffer.concat([disc("consume_credits"), le64(amount), receipt]);
  const consumeKeys = [m(config, false, false), m(credits, false, true), m(owner, true, false)];
  const t0 = Date.now();
  for (let i = 0; i < N; i++) {
    const ix = new TransactionInstruction({ programId: PROG, keys: consumeKeys, data: consumeData(AMOUNT) });
    await send(er, ix, `consume ${i + 1}/${N}`);
  }
  const ms = Date.now() - t0;
  console.log(`  ${N} consume in ER: ${ms} ms total, ${(ms / N).toFixed(1)} ms/op`);

  // 3) undelegate (commit final + return to L1).
  console.log(`\n[3] undelegate_credits (ER)`);
  const undelIx = new TransactionInstruction({
    programId: PROG,
    keys: [
      m(owner, true, true),               // owner
      m(credits, false, true),            // credits
      m(MAGIC_PROGRAM_ID, false, false),  // magic_program
      m(MAGIC_CONTEXT_ID, false, true),   // magic_context
    ],
    data: disc("undelegate_credits"),
  });
  await send(er, undelIx, "undelegate");
  console.log("  waiting for L1 finalization of the commit+undelegate...");
  await sleep(12000);

  // 4) reconcile on L1.
  console.log(`\n[4] reconcile on L1`);
  const after = await balanceL1();
  const expected = before - AMOUNT * BigInt(N);
  console.log(`  before ${before} | after ${after} | expected ${expected}`);
  if (after !== expected) throw new Error(`RECONCILE FAILED: ${after} != ${expected}`);
  console.log(`\nRECONCILE OK — ${N} ER-metered consumes reconciled exactly to L1.`);
}

function le64(v) { const b = Buffer.alloc(8); b.writeBigUInt64LE(BigInt(v)); return b; }
main().catch((e) => { console.error("\nSPIKE FAILED:", e.message || e); process.exit(1); });
