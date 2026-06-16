// Experiment for Q4: how is a transaction handled when its writable set spans a
// delegated account (credits, in the ER) AND a non-delegated L1 state change
// (a SystemProgram.transfer between normal accounts)? Send the same mixed tx to
// the Magic Router, the ER validator, and L1, and report each outcome.

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
const VALIDATOR = new PublicKey("MEUGGrYPxKk17hCr7wpT6s8dtNokZj5U2L57vjYMS8e");
const payer = Keypair.fromSecretKey(new Uint8Array(JSON.parse(
  fs.readFileSync(`${os.homedir()}/.config/solana/id.json`, "utf8"))));
const owner = payer.publicKey;
const throwaway = Keypair.generate().publicKey;

const l1 = new Connection("https://api.devnet.solana.com", "confirmed");
const er = new Connection("https://devnet-eu.magicblock.app", "confirmed");
const router = new Connection("https://devnet-router.magicblock.app", "confirmed");

const pda = (s) => PublicKey.findProgramAddressSync(s, PROG)[0];
const config = pda([Buffer.from("config")]);
const credits = pda([Buffer.from("credits"), owner.toBuffer()]);
const disc = (n) => crypto.createHash("sha256").update(`global:${n}`).digest().subarray(0, 8);
const le64 = (v) => { const b = Buffer.alloc(8); b.writeBigUInt64LE(BigInt(v)); return b; };
const m = (pubkey, s, w) => ({ pubkey, isSigner: s, isWritable: w });
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

const consumeIx = () => new TransactionInstruction({
  programId: PROG,
  keys: [m(config, false, false), m(credits, false, true), m(owner, true, false)],
  data: Buffer.concat([disc("consume_credits"), le64(1n), Buffer.alloc(32)]),
});
// mixed: delegated write (credits) + a real non-delegated L1 lamport move.
const mixedTx = () => new Transaction()
  .add(consumeIx())
  .add(SystemProgram.transfer({ fromPubkey: owner, toPubkey: throwaway, lamports: 1000 }));

async function isDelegated() {
  const ai = await l1.getAccountInfo(credits);
  return ai && ai.owner.equals(DELEGATION_PROGRAM_ID);
}

async function attempt(name, conn, txFactory) {
  try {
    const sig = await sendAndConfirmTransaction(conn, txFactory(), [payer], { commitment: "confirmed" });
    console.log(`  ${name}: SUCCEEDED ${sig}`);
  } catch (e) {
    const logs = e?.logs ? "\n      " + e.logs.slice(0, 4).join("\n      ") : "";
    console.log(`  ${name}: REJECTED -> ${(e.message || e).toString().split("\n")[0]}${logs}`);
  }
}

async function main() {
  console.log(`credits ${credits} | throwaway ${throwaway}`);
  if (!(await isDelegated())) {
    console.log("delegating credits to EU validator first...");
    await sendAndConfirmTransaction(l1, new Transaction().add(new TransactionInstruction({
      programId: PROG,
      keys: [
        m(owner, true, true),
        m(delegateBufferPdaFromDelegatedAccountAndOwnerProgram(credits, PROG), false, true),
        m(delegationRecordPdaFromDelegatedAccount(credits), false, true),
        m(delegationMetadataPdaFromDelegatedAccount(credits), false, true),
        m(credits, false, true), m(PROG, false, false),
        m(DELEGATION_PROGRAM_ID, false, false), m(SystemProgram.programId, false, false),
        m(VALIDATOR, false, false),
      ],
      data: disc("delegate_credits"),
    })), [payer], { commitment: "confirmed" });
    await sleep(6000);
  }
  console.log(`delegated: ${await isDelegated()}\n`);

  console.log("[control] pure consume_credits (delegated-only writable set):");
  await attempt("-> ER    ", er, () => new Transaction().add(consumeIx()));
  await attempt("-> L1    ", l1, () => new Transaction().add(consumeIx()));

  console.log("\n[mixed] consume_credits (delegated) + system transfer (non-delegated L1 write):");
  await attempt("-> router", router, mixedTx);
  await attempt("-> ER    ", er, mixedTx);
  await attempt("-> L1    ", l1, mixedTx);

  console.log("\ncleanup: undelegate");
  try {
    await sendAndConfirmTransaction(er, new Transaction().add(new TransactionInstruction({
      programId: PROG,
      keys: [m(owner, true, true), m(credits, false, true), m(MAGIC_PROGRAM_ID, false, false), m(MAGIC_CONTEXT_ID, false, true)],
      data: disc("undelegate_credits"),
    })), [payer], { commitment: "confirmed" });
    console.log("  undelegated.");
  } catch (e) { console.log("  undelegate err:", (e.message || e).toString().split("\n")[0]); }
}
main().catch((e) => { console.error("FATAL:", e.message || e); process.exit(1); });
