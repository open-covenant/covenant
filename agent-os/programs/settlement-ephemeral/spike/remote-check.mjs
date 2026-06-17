// Pay a remote (deployed) x402 ER facilitator end to end: GET /paid -> 402 ->
// settle by consume_credits in the ER (receipt_hash = sha256(nonce string), matching
// the Rust facilitator/EphemeralSigner) -> retry with x-payment -> expect 200.
// The credit account must already be delegated. Env: URL (the facilitator /paid).

import fs from "node:fs";
import os from "node:os";
import crypto from "node:crypto";
import { Connection, Keypair, PublicKey, Transaction, TransactionInstruction, sendAndConfirmTransaction } from "@solana/web3.js";

const PROG = new PublicKey("cov9UDypG7nsryxdgMcKhKU2spRVWLVjxT2iTv6do5Y");
const URL = process.env.URL || "https://covenant-x402-er-facilitator.onrender.com/paid";
const ER = process.env.ER || "https://devnet-eu.magicblock.app";
const payer = Keypair.fromSecretKey(new Uint8Array(JSON.parse(
  fs.readFileSync(process.env.PAYER || `${os.homedir()}/.config/solana/id.json`, "utf8"))));
const owner = payer.publicKey;
const er = new Connection(ER, "confirmed");
const pda = (s) => PublicKey.findProgramAddressSync(s, PROG)[0];
const config = pda([Buffer.from("config")]);
const credits = pda([Buffer.from("credits"), owner.toBuffer()]);
const disc = (n) => crypto.createHash("sha256").update(`global:${n}`).digest().subarray(0, 8);
const le64 = (v) => { const b = Buffer.alloc(8); b.writeBigUInt64LE(BigInt(v)); return b; };
const m = (pubkey, s, w) => ({ pubkey, isSigner: s, isWritable: w });

async function main() {
  console.log(`calling deployed facilitator: ${URL}`);
  const r1 = await fetch(URL);
  if (r1.status !== 402) throw new Error(`expected 402, got ${r1.status}`);
  const arr = await r1.json();
  const ch = Array.isArray(arr) ? arr[0] : arr;
  const nonce = ch.extra.nonce;
  const amount = BigInt(ch.amount);
  console.log(`  402 challenge: amount ${amount} ${ch.asset}, nonce ${nonce}`);

  // receipt_hash = sha256(nonce string bytes) -- matches the Rust facilitator.
  const receipt = crypto.createHash("sha256").update(nonce, "utf8").digest();
  const ix = new TransactionInstruction({
    programId: PROG,
    keys: [m(config, false, false), m(credits, false, true), m(owner, true, false)],
    data: Buffer.concat([disc("consume_credits"), le64(amount), receipt]),
  });
  const sig = await sendAndConfirmTransaction(er, new Transaction().add(ix), [payer], { commitment: "confirmed" });
  console.log(`  paid in ER: ${sig}`);

  const header = Buffer.from(JSON.stringify({ x402Version: 1, scheme: "exact-er", payload: { signature: sig, nonce } })).toString("base64");
  const r2 = await fetch(URL, { headers: { "x-payment": header } });
  const body = await r2.text();
  console.log(`  retry with x-payment -> ${r2.status}: ${body}`);
  if (r2.status !== 200) throw new Error(`paid retry got ${r2.status}`);
  console.log("\nOK — deployed facilitator verified a live ER-settled payment.");
}
main().catch((e) => { console.error("FAILED:", e.message || e); process.exit(1); });
