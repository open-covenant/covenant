#!/usr/bin/env node
// Drives covenant-compute-lease-signer on Solana devnet exactly the way the
// coordinator does: one process per step, a JSON request on stdin, one JSON
// line back on stdout. Three leases, each checked against chain state rather
// than against what the signer reported:
//
//   1. open, a run of ticks, conclude: the operator is paid the billed
//      elapsed, the renter gets the rest, and the committed provenance root
//      is the hash chain of the ticks that landed.
//   2. open, tick, void: the whole vault goes back and the operator is paid
//      nothing.
//   3. open, tick past the elapsed the conclusion bills: the signer refuses
//      to settle a meter that disagrees with the bill, voids the lease, and
//      says the payout stays off chain.
//
// Plus the refusals that cost nothing: an endpoint that is not the pinned
// validator, and a second conclude that must answer with the first.
//
// Escrow is a throwaway 6-decimal mint this script creates; devnet USDC is not
// freely mintable.
//
// Usage: node lease-signer-e2e.mjs
//        RPC=<devnet rpc> ER=https://devnet-eu.magicblock.app SIGNER=<binary> node lease-signer-e2e.mjs

import fs from "node:fs";
import path from "node:path";
import crypto from "node:crypto";
import { spawn } from "node:child_process";
import { fileURLToPath } from "node:url";
import {
  Connection,
  Keypair,
  LAMPORTS_PER_SOL,
  PublicKey,
  SystemProgram,
  Transaction,
  sendAndConfirmTransaction,
} from "@solana/web3.js";
import { createMint, getAccount, getOrCreateAssociatedTokenAccount, mintTo } from "@solana/spl-token";

const HERE = path.dirname(fileURLToPath(import.meta.url));
const REPO = path.resolve(HERE, "..", "..", "..");
const KEYDIR = path.join(REPO, "scratchpad", "compute-mainnet");
const SIGNER =
  process.env.SIGNER ||
  path.join(REPO, "agent-os/crates/covenant-compute-lease-signer/target/release/covenant-compute-lease-signer");

const PROGRAM_ID = new PublicKey(process.env.PROGRAM || "CLSeVNrRi4TpXsXAkAuLh58kGCCAd1w1bj2CcEhTEESd");
const L1_URL = process.env.RPC || "https://api.devnet.solana.com";
const ER_URL = process.env.ER || "https://devnet-eu.magicblock.app";
const VALIDATOR = new PublicKey(process.env.VALIDATOR || "MEUGGrYPxKk17hCr7wpT6s8dtNokZj5U2L57vjYMS8e");
const OTHER_VALIDATOR = "MUS3hc9TCw4cGC12vHNoYcCGzJG1txjgQLZWVoeNHNd";

const RATE = 100n; // micro-units per second
const WINDOW = 600n; // seconds
const DELEGATION_PROGRAM = new PublicKey("DELeGGvXpWV2fqJUhqcF5ZSYMS4JTLjteaAMARRSaeSh");

const l1 = new Connection(L1_URL, "confirmed");
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const sha256 = (...parts) => crypto.createHash("sha256").update(Buffer.concat(parts)).digest();
const pda = (seeds) => PublicKey.findProgramAddressSync(seeds, PROGRAM_ID)[0];

function fail(message) {
  throw new Error(message);
}

function assertEq(label, actual, expected) {
  const a = typeof actual === "bigint" ? actual.toString() : String(actual);
  const e = typeof expected === "bigint" ? expected.toString() : String(expected);
  if (a !== e) fail(`${label}: got ${a}, expected ${e}`);
  console.log(`  ok  ${label} = ${a}`);
}

function key(name) {
  fs.mkdirSync(KEYDIR, { recursive: true, mode: 0o700 });
  const file = path.join(KEYDIR, `${name}.json`);
  if (!fs.existsSync(file)) {
    fs.writeFileSync(file, JSON.stringify(Array.from(Keypair.generate().secretKey)), { mode: 0o600 });
  }
  return { file, keypair: Keypair.fromSecretKey(new Uint8Array(JSON.parse(fs.readFileSync(file, "utf8")))) };
}

function runSigner(step, request, renter, coordinator) {
  return new Promise((resolve, reject) => {
    const child = spawn(SIGNER, [step], {
      env: {
        COVENANT_COMPUTE_LEASE_KEYPAIR: renter.file,
        COVENANT_COMPUTE_LEASE_COORDINATOR_KEYPAIR: coordinator.file,
        COVENANT_COMPUTE_LEASE_RPC_URL: L1_URL,
        COVENANT_COMPUTE_LEASE_ER_RPC_URL: ER_URL,
      },
      stdio: ["pipe", "pipe", "pipe"],
    });
    let stdout = "";
    let stderr = "";
    child.stdout.on("data", (d) => (stdout += d));
    child.stderr.on("data", (d) => (stderr += d));
    child.on("error", reject);
    child.on("close", (code) => {
      const line = stdout.trim().split("\n").at(-1) || "{}";
      resolve({ code, body: JSON.parse(line), stderr: stderr.trim() });
    });
    child.stdin.end(JSON.stringify(request));
  });
}

function decodeTerms(data) {
  return {
    paidOperator: data[216] === 1,
    paidRenter: data[217] === 1,
    voided: data[218] === 1,
    delegated: data[219] === 1,
  };
}

function decodeMeter(data) {
  return {
    meteredMs: data.readBigUInt64LE(88),
    root: Buffer.from(data.subarray(96, 128)),
    concluded: data[128] === 1,
  };
}

async function balance(address) {
  return (await getAccount(l1, address, "confirmed")).amount;
}

// ------------------------------------------------------------------- setup

console.log("covenant-compute-lease-signer against devnet\n");
console.log(`  signer     ${SIGNER}`);
console.log(`  program    ${PROGRAM_ID.toBase58()}`);
console.log(`  rollup     ${ER_URL} (${VALIDATOR.toBase58()})`);
if (!fs.existsSync(SIGNER)) fail("build the signer first: cargo build --release in its crate");

const renter = key("devnet-signer-renter");
const coordinator = key("devnet-signer-coordinator");
const operator = key("devnet-signer-operator");
console.log(`  renter     ${renter.keypair.publicKey.toBase58()}`);
console.log(`  coordinator ${coordinator.keypair.publicKey.toBase58()} (holds no SOL, only signs)`);
console.log(`  operator   ${operator.keypair.publicKey.toBase58()}`);

const want = Math.round(0.08 * LAMPORTS_PER_SOL);
const have = await l1.getBalance(renter.keypair.publicKey, "confirmed");
if (have < want) {
  const funder = key("devnet-deployer").keypair;
  await sendAndConfirmTransaction(
    l1,
    new Transaction().add(
      SystemProgram.transfer({ fromPubkey: funder.publicKey, toPubkey: renter.keypair.publicKey, lamports: want - have }),
    ),
    [funder],
    { commitment: "confirmed" },
  );
}
console.log(`  renter SOL ${(await l1.getBalance(renter.keypair.publicKey, "confirmed")) / LAMPORTS_PER_SOL}`);

const mint = await createMint(l1, renter.keypair, renter.keypair.publicKey, null, 6, undefined, {
  commitment: "confirmed",
});
const renterAta = (await getOrCreateAssociatedTokenAccount(l1, renter.keypair, mint, renter.keypair.publicKey, false, "confirmed")).address;
await mintTo(l1, renter.keypair, mint, renterAta, renter.keypair, 1_000_000n, [], { commitment: "confirmed" });
console.log(`  mint       ${mint.toBase58()} (stand-in for USDC)`);

const envelope = (jobId, validator = VALIDATOR.toBase58()) => ({
  program_id: PROGRAM_ID.toBase58(),
  mint: mint.toBase58(),
  er_validator: validator,
  job_id: jobId,
});

const openRequest = (jobId, validator) => ({
  ...envelope(jobId, validator),
  renter: Keypair.generate().publicKey.toBase58(),
  operator: operator.keypair.publicKey.toBase58(),
  rate_micro_usdc_per_sec: Number(RATE),
  max_duration_secs: Number(WINDOW),
  accepted_at_ms: Date.now(),
});

function addresses(jobId) {
  const terms = pda([Buffer.from("lease"), renter.keypair.publicKey.toBuffer(), Buffer.from(jobId, "hex")]);
  return { terms, meter: pda([Buffer.from("meter"), terms.toBuffer()]), vault: pda([Buffer.from("vault"), terms.toBuffer()]) };
}

async function open(jobId) {
  const result = await runSigner("lease-open", openRequest(jobId), renter, coordinator);
  if (result.code !== 0) fail(`lease-open: ${JSON.stringify(result.body)}\n${result.stderr}`);
  const { terms } = addresses(jobId);
  const state = decodeTerms((await l1.getAccountInfo(terms, "confirmed")).data);
  assertEq("lease open and its meter delegated", state.delegated, true);
  return result.body.signature;
}

// Ticks the way the coordinator's pass does: a failed tick is tolerated and
// the next one carries the cumulative total. Only landed ticks enter the
// provenance chain, so only those are recorded.
async function tick(jobId, meteredMs, landed) {
  const receipt = sha256(Buffer.from(jobId, "hex"), Buffer.from(String(meteredMs)));
  const result = await runSigner(
    "lease-tick",
    { ...envelope(jobId), metered_ms: meteredMs, receipt_hash_hex: receipt.toString("hex") },
    renter,
    coordinator,
  );
  if (result.code === 0 && result.body.signature) {
    landed.push(receipt);
    return true;
  }
  console.log(`  tick ${meteredMs} ms not landed: ${JSON.stringify(result.body)}`);
  return false;
}

async function tickRun(jobId, totals) {
  const landed = [];
  for (const total of totals) {
    for (let attempt = 0; attempt < 5; attempt++) {
      if (await tick(jobId, total, landed)) break;
      await sleep(1500);
    }
  }
  if (landed.length === 0) fail("no tick landed");
  console.log(`  ${landed.length}/${totals.length} ticks landed in the rollup`);
  return landed;
}

// ----------------------------------------------------------------- refusal

console.log("\n0. an endpoint that is not the pinned validator");
{
  const jobId = crypto.randomBytes(16).toString("hex");
  const result = await runSigner("lease-open", openRequest(jobId, OTHER_VALIDATOR), renter, coordinator);
  assertEq("open refused", result.code, 1);
  assertEq("refusal is safe to retry", result.body.stage, "not_submitted");
  assertEq("no lease was created", (await l1.getAccountInfo(addresses(jobId).terms, "confirmed")) === null, true);
}

// ------------------------------------------------------------------ settle

console.log("\n1. open, tick, conclude");
const settleJob = crypto.randomBytes(16).toString("hex");
const start = await balance(renterAta);
await open(settleJob);
const landed = await tickRun(settleJob, [1000, 2000, 3000, 4000, 5000]);
const billedMs = 6000;
const finalReceipt = sha256(Buffer.from(settleJob, "hex"), Buffer.from("final"));
const payoutMemo = `compute-payout:v1:${settleJob}:e2e`;
const concluded = await runSigner(
  "lease-conclude",
  {
    ...envelope(settleJob),
    metered_ms: billedMs,
    receipt_hash_hex: finalReceipt.toString("hex"),
    payout_memo: payoutMemo,
  },
  renter,
  coordinator,
);
if (concluded.code !== 0) fail(`lease-conclude: ${JSON.stringify(concluded.body)}\n${concluded.stderr}`);
console.log(`  settle tx  ${concluded.body.signature}`);
{
  const { terms, meter, vault } = addresses(settleJob);
  const charged = (RATE * BigInt(billedMs) + 999n) / 1000n;
  const operatorAta = (await getOrCreateAssociatedTokenAccount(l1, renter.keypair, mint, operator.keypair.publicKey, false, "confirmed")).address;
  assertEq("operator paid the billed elapsed", await balance(operatorAta), charged);
  assertEq("renter keeps the rest", await balance(renterAta), start - charged);
  const state = decodeTerms((await l1.getAccountInfo(terms, "confirmed")).data);
  assertEq("both sides paid", state.paidOperator && state.paidRenter, true);
  assertEq("vault closed", (await l1.getAccountInfo(vault, "confirmed")) === null, true);
  const committed = decodeMeter((await l1.getAccountInfo(meter, "confirmed")).data);
  assertEq("meter committed the billed elapsed", committed.meteredMs, BigInt(billedMs));
  assertEq("meter closed", committed.concluded, true);
  const root = [...landed, finalReceipt].reduce((acc, r) => sha256(acc, r), Buffer.alloc(32));
  assertEq("provenance root is the chain of landed ticks", committed.root.toString("hex"), root.toString("hex"));
  // The payment the signer reports reads as one payout: the memo, and one
  // wallet credited. The renter's remainder moved in a transaction of its own.
  const paid = await l1.getParsedTransaction(concluded.body.signature, {
    commitment: "confirmed",
    maxSupportedTransactionVersion: 0,
  });
  const memos = paid.transaction.message.instructions
    .filter((ix) => ix.program === "spl-memo")
    .map((ix) => ix.parsed);
  assertEq("the payment carries the payout memo", JSON.stringify(memos), JSON.stringify([payoutMemo]));
  const grew = (paid.meta.postTokenBalances || []).filter((post) => {
    const pre = (paid.meta.preTokenBalances || []).find((b) => b.accountIndex === post.accountIndex);
    return BigInt(post.uiTokenAmount.amount) > BigInt(pre ? pre.uiTokenAmount.amount : "0");
  });
  assertEq("one wallet credited by the payment", grew.map((g) => g.owner).join(","), operator.keypair.publicKey.toBase58());
  const again = await runSigner(
    "lease-conclude",
    {
      ...envelope(settleJob),
      metered_ms: billedMs,
      receipt_hash_hex: finalReceipt.toString("hex"),
      payout_memo: payoutMemo,
    },
    renter,
    coordinator,
  );
  assertEq("a repeated conclude succeeds without paying twice", again.code, 0);
  assertEq("and answers with the same payment", again.body.signature, concluded.body.signature);
  assertEq("operator balance unchanged by the repeat", await balance(operatorAta), charged);
}

// -------------------------------------------------------------------- void

console.log("\n2. open, tick, void");
const voidJob = crypto.randomBytes(16).toString("hex");
const beforeVoid = await balance(renterAta);
await open(voidJob);
await tickRun(voidJob, [1000, 2000]);
const voided = await runSigner("lease-void", envelope(voidJob), renter, coordinator);
if (voided.code !== 0) fail(`lease-void: ${JSON.stringify(voided.body)}\n${voided.stderr}`);
{
  const state = decodeTerms((await l1.getAccountInfo(addresses(voidJob).terms, "confirmed")).data);
  assertEq("lease voided", state.voided, true);
  assertEq("renter refunded in full", await balance(renterAta), beforeVoid);
  const late = await runSigner(
    "lease-conclude",
    { ...envelope(voidJob), metered_ms: 2000, receipt_hash_hex: "00".repeat(32) },
    renter,
    coordinator,
  );
  assertEq("conclude after a void refused", late.code, 1);
  assertEq("and says the payout stays off chain", late.body.stage, "not_submitted");
}

// ------------------------------------------------------------------- race

console.log("\n3. a rollup meter ahead of the bill");
const raceJob = crypto.randomBytes(16).toString("hex");
const beforeRace = await balance(renterAta);
await open(raceJob);
await tickRun(raceJob, [5000]);
const race = await runSigner(
  "lease-conclude",
  { ...envelope(raceJob), metered_ms: 3000, receipt_hash_hex: "11".repeat(32) },
  renter,
  coordinator,
);
assertEq("conclude refused", race.code, 1);
assertEq("stage leaves the payout off chain", race.body.stage, "not_submitted");
console.log(`  reason     ${race.body.error}`);
{
  const { terms, meter } = addresses(raceJob);
  const state = decodeTerms((await l1.getAccountInfo(terms, "confirmed")).data);
  assertEq("lease voided instead of settled", state.voided, true);
  assertEq("renter refunded in full", await balance(renterAta), beforeRace);
  const home = await l1.getAccountInfo(meter, "confirmed");
  assertEq("meter never came home at the higher figure", home.owner.equals(DELEGATION_PROGRAM), true);
}

console.log("\nall lease-signer checks passed");
