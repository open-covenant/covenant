// Tiny session helper for live verification: delegate / undelegate / balance.
//   node er-session.mjs delegate|undelegate|balance
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
const l1 = new Connection("https://api.devnet.solana.com", "confirmed");
const er = new Connection("https://devnet-eu.magicblock.app", "confirmed");
const pda = (s) => PublicKey.findProgramAddressSync(s, PROG)[0];
const credits = pda([Buffer.from("credits"), owner.toBuffer()]);
const disc = (n) => crypto.createHash("sha256").update(`global:${n}`).digest().subarray(0, 8);
const m = (pubkey, s, w) => ({ pubkey, isSigner: s, isWritable: w });

async function bal(conn) { const ai = await conn.getAccountInfo(credits); return ai ? ai.data.readBigUInt64LE(40) : null; }

const action = process.argv[2];
if (action === "balance") {
  console.log(`L1 ${await bal(l1)} | ER ${await bal(er).catch(() => "n/a")}`);
} else if (action === "delegate") {
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
  console.log("delegated to EU");
} else if (action === "undelegate") {
  await sendAndConfirmTransaction(er, new Transaction().add(new TransactionInstruction({
    programId: PROG,
    keys: [m(owner, true, true), m(credits, false, true), m(MAGIC_PROGRAM_ID, false, false), m(MAGIC_CONTEXT_ID, false, true)],
    data: disc("undelegate_credits"),
  })), [payer], { commitment: "confirmed" });
  console.log("undelegated");
} else {
  console.error("usage: node er-session.mjs delegate|undelegate|balance");
  process.exit(1);
}
