// x402-over-ER demo: a paid HTTP endpoint settled by gasless credit metering in a
// MagicBlock ephemeral rollup, end to end and live on devnet.
//
//   - facilitator (HTTP): GET /quote returns 402 + a challenge {amount, nonce} until
//     a valid x-payment header is presented; then 200 + the content.
//   - agent (client): on 402, settles by running consume_credits in the ER with
//     receipt_hash = sha256(nonce) (binds the payment to this request), then retries
//     with an x-payment envelope carrying the ER tx signature.
//   - the facilitator verifies the signature ON THE ER: right program, right credit
//     account, amount >= price, receipt_hash == sha256(nonce), and the signature is
//     unused (anti-replay). No SPL transfer, no per-call L1 fee.
//
// Session model: delegate the credit account once, serve K paid calls each settled
// gaslessly in the ER, undelegate, reconcile on L1.
//
// Env: PAYER (keypair, default id.json), K (paid calls, default 5), PRICE (credits/call,
//      default 1), VALIDATOR (pinned ER validator, default EU), ER (validator RPC).

import fs from "node:fs";
import os from "node:os";
import http from "node:http";
import crypto from "node:crypto";
import {
  Connection, Keypair, PublicKey, SystemProgram,
  Transaction, TransactionInstruction, sendAndConfirmTransaction,
} from "@solana/web3.js";
import bs58 from "bs58";
import { DELEGATION_PROGRAM_ID, MAGIC_PROGRAM_ID, MAGIC_CONTEXT_ID,
  delegateBufferPdaFromDelegatedAccountAndOwnerProgram,
  delegationRecordPdaFromDelegatedAccount,
  delegationMetadataPdaFromDelegatedAccount } from "@magicblock-labs/ephemeral-rollups-sdk";

const PROG = new PublicKey("cov9UDypG7nsryxdgMcKhKU2spRVWLVjxT2iTv6do5Y");
const env = (k, d) => process.env[k] ?? d;
const K = Number(env("K", "5"));
const PRICE = BigInt(env("PRICE", "1"));
const ER_URL = env("ER", "https://devnet-eu.magicblock.app");
const VALIDATOR = new PublicKey(env("VALIDATOR", "MEUGGrYPxKk17hCr7wpT6s8dtNokZj5U2L57vjYMS8e"));
const PORT = 8402;

const payer = Keypair.fromSecretKey(new Uint8Array(JSON.parse(
  fs.readFileSync(env("PAYER", `${os.homedir()}/.config/solana/id.json`), "utf8"))));
const owner = payer.publicKey;
const l1 = new Connection("https://api.devnet.solana.com", "confirmed");
const er = new Connection(ER_URL, "confirmed");
const pda = (s) => PublicKey.findProgramAddressSync(s, PROG)[0];
const config = pda([Buffer.from("config")]);
const credits = pda([Buffer.from("credits"), owner.toBuffer()]);
const disc = (n) => crypto.createHash("sha256").update(`global:${n}`).digest().subarray(0, 8);
const sha = (b) => crypto.createHash("sha256").update(b).digest();
const m = (pubkey, isSigner, isWritable) => ({ pubkey, isSigner, isWritable });
const le64 = (v) => { const b = Buffer.alloc(8); b.writeBigUInt64LE(BigInt(v)); return b; };
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const CONSUME_DISC = disc("consume_credits");

async function erBalance() {
  const ai = await er.getAccountInfo(credits);
  return ai ? ai.data.readBigUInt64LE(40) : null;
}

// ---- facilitator: verifies an ER consume_credits as proof of payment ----
const usedSigs = new Set();
async function verifyPayment({ signature, nonce }) {
  if (usedSigs.has(signature)) return { ok: false, why: "signature replayed" };
  const tx = await er.getTransaction(signature, { commitment: "confirmed", maxSupportedTransactionVersion: 0 });
  if (!tx) return { ok: false, why: "tx not found on ER" };
  if (tx.meta?.err) return { ok: false, why: "tx failed" };
  const msg = tx.transaction.message;
  const keys = msg.staticAccountKeys ?? msg.accountKeys;
  const ixs = msg.compiledInstructions ?? msg.instructions;
  for (const ix of ixs) {
    if (!keys[ix.programIdIndex].equals(PROG)) continue;
    const data = Buffer.from(typeof ix.data === "string" ? bs58.decode(ix.data) : ix.data);
    if (!data.subarray(0, 8).equals(CONSUME_DISC)) continue;
    const amount = data.readBigUInt64LE(8);
    const receipt = data.subarray(16, 48);
    const acctIdx = ix.accountKeyIndexes ?? ix.accounts; // [config, credits, owner]
    const creditAcct = keys[acctIdx[1]];
    if (!creditAcct.equals(credits)) return { ok: false, why: "wrong credit account" };
    if (amount < PRICE) return { ok: false, why: `underpaid ${amount} < ${PRICE}` };
    if (!receipt.equals(sha(Buffer.from(nonce, "hex")))) return { ok: false, why: "receipt_hash != sha256(nonce)" };
    usedSigs.add(signature);
    return { ok: true, amount };
  }
  return { ok: false, why: "no consume_credits to our program in tx" };
}

function startFacilitator() {
  return new Promise((resolve) => {
    const server = http.createServer(async (req, res) => {
      const hdr = req.headers["x-payment"];
      const send = (code, obj, extra = {}) =>
        res.writeHead(code, { "content-type": "application/json", ...extra }).end(JSON.stringify(obj));
      if (!hdr) {
        const nonce = crypto.randomBytes(16).toString("hex");
        return send(402, {
          x402Version: 1, scheme: "exact-er", network: "solana-er:devnet",
          program: PROG.toBase58(), creditAccount: credits.toBase58(),
          amountCredits: PRICE.toString(), nonce,
        });
      }
      let proof;
      try { proof = JSON.parse(Buffer.from(hdr, "base64").toString()); }
      catch { return send(400, { error: "bad x-payment" }); }
      const v = await verifyPayment(proof);
      if (!v.ok) return send(402, { error: "payment invalid", why: v.why });
      send(200, { quote: "BTC looks coiled. Position for a breakout.", paidCredits: v.amount.toString() });
    });
    server.listen(PORT, () => resolve(server));
  });
}

// ---- agent: pays per call by metering in the ER ----
async function paidGet(url) {
  const r1 = await fetch(url);
  if (r1.status !== 402) throw new Error(`expected 402, got ${r1.status}`);
  const ch = await r1.json();
  const receipt = sha(Buffer.from(ch.nonce, "hex"));
  const ix = new TransactionInstruction({
    programId: PROG,
    keys: [m(config, false, false), m(credits, false, true), m(owner, true, false)],
    data: Buffer.concat([CONSUME_DISC, le64(BigInt(ch.amountCredits)), receipt]),
  });
  const sig = await sendAndConfirmTransaction(er, new Transaction().add(ix), [payer], { commitment: "confirmed" });
  const header = Buffer.from(JSON.stringify({ signature: sig, nonce: ch.nonce })).toString("base64");
  const r2 = await fetch(url, { headers: { "x-payment": header } });
  if (r2.status !== 200) throw new Error(`paid retry got ${r2.status}: ${await r2.text()}`);
  return { body: await r2.json(), sig };
}

async function send(conn, ix, label) {
  const sig = await sendAndConfirmTransaction(conn, new Transaction().add(ix), [payer], { commitment: "confirmed" });
  console.log(`  ${label}: ${sig}`);
}

async function main() {
  const server = await startFacilitator();
  const url = `http://127.0.0.1:${PORT}/quote`;
  console.log(`facilitator up on ${url} | program ${PROG} | credits ${credits}`);

  console.log(`\n[1] delegate credit account to ER (pin ${VALIDATOR})`);
  await send(l1, new TransactionInstruction({
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
  }), "delegate");
  await sleep(6000);

  const before = await erBalance();
  console.log(`\n[2] ${K} paid API calls, each settled gasless in the ER (ER balance ${before})`);
  const lat = [];
  for (let i = 0; i < K; i++) {
    const t = Date.now();
    const { body, sig } = await paidGet(url);
    lat.push(Date.now() - t);
    console.log(`  call ${i + 1}/${K}: 402 -> ER pay ${sig.slice(0, 8)}.. -> 200 "${body.quote.slice(0, 28)}.." (${lat[i]}ms)`);
  }
  const afterEr = await erBalance();
  const avg = (lat.reduce((a, b) => a + b, 0) / K).toFixed(0);
  console.log(`  paid calls: ${K}, ER balance ${before} -> ${afterEr} (-${before - afterEr}), ${avg}ms/call end-to-end`);

  console.log(`\n[3] undelegate + reconcile on L1`);
  await send(er, new TransactionInstruction({
    programId: PROG,
    keys: [m(owner, true, true), m(credits, false, true), m(MAGIC_PROGRAM_ID, false, false), m(MAGIC_CONTEXT_ID, false, true)],
    data: disc("undelegate_credits"),
  }), "undelegate");
  await sleep(12000);
  const l1ai = await l1.getAccountInfo(credits);
  const l1bal = l1ai.data.readBigUInt64LE(40);
  const expected = before - PRICE * BigInt(K);
  console.log(`  L1 balance ${l1bal} | expected ${expected}`);
  server.close();
  if (l1bal !== expected) throw new Error(`RECONCILE FAILED: ${l1bal} != ${expected}`);
  console.log(`\nOK — ${K} x402 calls settled gaslessly in the ER and reconciled exactly to L1.`);
}

main().catch((e) => { console.error("\nDEMO FAILED:", e.message || e); process.exit(1); });
