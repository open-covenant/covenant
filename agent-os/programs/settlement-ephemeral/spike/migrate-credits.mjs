// Post-upgrade migration: realloc a legacy 49-byte CreditAccount to the new
// 81-byte layout (zero-fills provenance_root). Owner-gated, idempotent. Run once
// per account after the settlement program upgrade ships provenance_root.
//   KEYPAIR=~/.config/solana/solana-id.json node migrate-credits.mjs   # migrate DrawYGmd (8xbXHA)
import { Connection, Keypair, PublicKey, Transaction, TransactionInstruction, SystemProgram, sendAndConfirmTransaction } from "@solana/web3.js";
import crypto from "node:crypto";
import fs from "node:fs";
import os from "node:os";

const PROG = new PublicKey("cov9UDypG7nsryxdgMcKhKU2spRVWLVjxT2iTv6do5Y");
const NEW_LEN = 81; // 8 disc + 32 owner + 8 balance + 1 bump + 32 provenance_root
const RPC = (() => { try { return fs.readFileSync("/tmp/cov-deploy-rpc", "utf8").trim(); } catch { return process.env.L1 || "https://solana-rpc.publicnode.com"; } })();
const c = new Connection(RPC, "confirmed");

const path = (process.env.KEYPAIR || `${os.homedir()}/.config/solana/solana-id.json`).replace(/^~/, os.homedir());
const kp = Keypair.fromSecretKey(new Uint8Array(JSON.parse(fs.readFileSync(path, "utf8"))));
const disc = (n) => crypto.createHash("sha256").update(`global:${n}`).digest().subarray(0, 8);
const credits = PublicKey.findProgramAddressSync([Buffer.from("credits"), kp.publicKey.toBuffer()], PROG)[0];

const before = await c.getAccountInfo(credits);
console.log("owner   :", kp.publicKey.toBase58());
console.log("credits :", credits.toBase58(), "| len", before ? before.data.length : "none");
if (!before) { console.log("no credit account for this owner; nothing to migrate"); process.exit(0); }
if (before.data.length >= NEW_LEN) { console.log("already migrated"); process.exit(0); }

const ix = new TransactionInstruction({
  programId: PROG,
  keys: [
    { pubkey: credits, isSigner: false, isWritable: true },
    { pubkey: kp.publicKey, isSigner: true, isWritable: true },
    { pubkey: SystemProgram.programId, isSigner: false, isWritable: false },
  ],
  data: disc("migrate_credit_account"),
});
const sig = await sendAndConfirmTransaction(c, new Transaction().add(ix), [kp], { commitment: "confirmed" });
const after = await c.getAccountInfo(credits);
console.log(`migrated: ${sig}`);
console.log(`len ${before.data.length} -> ${after.data.length}`);
