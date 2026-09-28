#!/usr/bin/env node
// Exercises everything in the lease program that decides money, against a
// local validator. No rollup: delegate and undelegate are the two
// instructions that need one, and every other path — the escrow, the
// authority checks on the meter, the settlement gate, the void and the two
// separable payout legs — runs on L1 and can be tested here in seconds.
//
// Half of what this asserts is refusals. A meter anyone can drive is worth
// nothing, so the run pins down who is turned away and with which error.
//
// Usage: solana-test-validator --reset --quiet \
//          --bpf-program CLSeVNrRi4TpXsXAkAuLh58kGCCAd1w1bj2CcEhTEESd \
//          ../target/deploy/covenant_compute_lease.so &
//        node lease-local.mjs

import crypto from "node:crypto";
import {
  Connection,
  Keypair,
  LAMPORTS_PER_SOL,
  PublicKey,
  SystemProgram,
  Transaction,
  TransactionInstruction,
  sendAndConfirmTransaction,
} from "@solana/web3.js";
import {
  TOKEN_PROGRAM_ID,
  createMint,
  getAccount,
  getOrCreateAssociatedTokenAccount,
  mintTo,
} from "@solana/spl-token";

const PROGRAM_ID = new PublicKey("CLSeVNrRi4TpXsXAkAuLh58kGCCAd1w1bj2CcEhTEESd");
const RPC = process.env.RPC || "http://127.0.0.1:8899";

const ERR_HAS_ONE = 2001;
const ERR_NOT_ENOUGH_KEYS = 3005;
const ERR_ALREADY_PAID = 6005;
const ERR_ALREADY_VOIDED = 6006;
const ERR_METER_WENT_BACKWARDS = 6008;
const ERR_LEASE_STILL_RUNNING = 6010;

const conn = new Connection(RPC, "confirmed");
const disc = (name) => crypto.createHash("sha256").update(`global:${name}`).digest().subarray(0, 8);
const meta = (pubkey, isSigner, isWritable) => ({ pubkey, isSigner, isWritable });
const pda = (seeds) => PublicKey.findProgramAddressSync(seeds, PROGRAM_ID)[0];
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

let checks = 0;
function fail(message) {
  throw new Error(message);
}
function ok(label) {
  checks += 1;
  console.log(`  ok  ${label}`);
}
function assertEq(label, actual, expected) {
  const a = typeof actual === "bigint" ? actual.toString() : String(actual);
  const e = typeof expected === "bigint" ? expected.toString() : String(expected);
  if (a !== e) fail(`${label}: got ${a}, expected ${e}`);
  ok(`${label} = ${a}`);
}

const send = (ix, signers) =>
  sendAndConfirmTransaction(conn, new Transaction().add(ix), signers, {
    commitment: "confirmed",
    skipPreflight: false,
  });

function customErrorCode(error) {
  const hay = [error?.message ?? "", ...(error?.logs ?? [])].join("\n");
  const hex = hay.match(/custom program error: 0x([0-9a-fA-F]+)/);
  if (hex) return Number.parseInt(hex[1], 16);
  const custom = error?.transactionError?.err?.InstructionError?.[1]?.Custom;
  return typeof custom === "number" ? custom : null;
}

async function refuses(label, expectedCode, ix, signers) {
  try {
    await send(ix, signers);
  } catch (e) {
    const code = customErrorCode(e);
    if (code !== expectedCode) fail(`${label}: rejected with ${code ?? e.message}, expected ${expectedCode}`);
    ok(`${label} (rejected with ${expectedCode})`);
    return;
  }
  fail(`${label}: the transaction was accepted`);
}

function decodeTerms(data) {
  return {
    renter: new PublicKey(data.subarray(24, 56)),
    coordinator: new PublicKey(data.subarray(88, 120)),
    erValidator: new PublicKey(data.subarray(120, 152)),
    rate: data.readBigUInt64LE(184),
    maxDuration: data.readBigUInt64LE(192),
    funded: data.readBigUInt64LE(200),
    openedAt: data.readBigInt64LE(208),
    paidOperator: data[216] === 1,
    paidRenter: data[217] === 1,
    voided: data[218] === 1,
    delegated: data[219] === 1,
  };
}

function decodeMeter(data) {
  return {
    meteredMs: data.readBigUInt64LE(88),
    provenanceRoot: Buffer.from(data.subarray(96, 128)),
    concluded: data[128] === 1,
  };
}

async function fund(kp, sol) {
  const sig = await conn.requestAirdrop(kp.publicKey, sol * LAMPORTS_PER_SOL);
  await conn.confirmTransaction(sig, "confirmed");
}

// ---------------------------------------------------------------- fixtures

const renter = Keypair.generate();
const operator = Keypair.generate();
const coordinator = Keypair.generate();
const stranger = Keypair.generate();
const validator = Keypair.generate().publicKey;

console.log(`Covenant Compute lease program, local validator at ${RPC}\n`);
if (!(await conn.getAccountInfo(PROGRAM_ID))) fail(`${PROGRAM_ID.toBase58()} is not loaded on ${RPC}`);

await Promise.all([fund(renter, 5), fund(operator, 1), fund(coordinator, 1), fund(stranger, 1)]);
const mint = await createMint(conn, renter, renter.publicKey, null, 6, undefined, { commitment: "confirmed" });
const renterAta = (await getOrCreateAssociatedTokenAccount(conn, renter, mint, renter.publicKey, false, "confirmed"))
  .address;
const operatorAta = (
  await getOrCreateAssociatedTokenAccount(conn, renter, mint, operator.publicKey, false, "confirmed")
).address;
await mintTo(conn, renter, mint, renterAta, renter, 10_000_000n, [], { commitment: "confirmed" });

// ------------------------------------------------------------ instructions

function lease(jobId) {
  const terms = pda([Buffer.from("lease"), renter.publicKey.toBuffer(), jobId]);
  return { jobId, terms, meter: pda([Buffer.from("meter"), terms.toBuffer()]), vault: pda([Buffer.from("vault"), terms.toBuffer()]) };
}

function openIx(l, rate, maxDuration, who = coordinator.publicKey) {
  const data = Buffer.alloc(40);
  disc("open_lease").copy(data, 0);
  l.jobId.copy(data, 8);
  data.writeBigUInt64LE(rate, 24);
  data.writeBigUInt64LE(maxDuration, 32);
  return new TransactionInstruction({
    programId: PROGRAM_ID,
    keys: [
      meta(renter.publicKey, true, true),
      meta(operator.publicKey, false, false),
      meta(who, false, false),
      meta(validator, false, false),
      meta(l.terms, false, true),
      meta(l.meter, false, true),
      meta(mint, false, false),
      meta(l.vault, false, true),
      meta(renterAta, false, true),
      meta(TOKEN_PROGRAM_ID, false, false),
      meta(SystemProgram.programId, false, false),
    ],
    data,
  });
}

function tickIx(l, meteredMs, receiptHash, signer) {
  const data = Buffer.alloc(48);
  disc("tick").copy(data, 0);
  data.writeBigUInt64LE(meteredMs, 8);
  receiptHash.copy(data, 16);
  const keys = [meta(l.meter, false, true)];
  if (signer) keys.push(meta(signer, true, false));
  return new TransactionInstruction({ programId: PROGRAM_ID, keys, data });
}

const settleIx = (l, payer) =>
  new TransactionInstruction({
    programId: PROGRAM_ID,
    keys: [
      meta(payer, true, true),
      meta(l.terms, false, true),
      meta(l.meter, false, false),
      meta(renter.publicKey, false, true),
      meta(mint, false, false),
      meta(l.vault, false, true),
      meta(operatorAta, false, true),
      meta(renterAta, false, true),
      meta(TOKEN_PROGRAM_ID, false, false),
    ],
    data: disc("settle_lease"),
  });

const voidIx = (l, signer) =>
  new TransactionInstruction({
    programId: PROGRAM_ID,
    keys: [meta(l.terms, false, true), meta(signer, true, false)],
    data: disc("void_lease"),
  });

const claimOperatorIx = (l, payer) =>
  new TransactionInstruction({
    programId: PROGRAM_ID,
    keys: [
      meta(payer, true, true),
      meta(l.terms, false, true),
      meta(l.meter, false, false),
      meta(mint, false, false),
      meta(l.vault, false, true),
      meta(operatorAta, false, true),
      meta(TOKEN_PROGRAM_ID, false, false),
    ],
    data: disc("claim_operator_share"),
  });

const claimRenterIx = (l, payer) =>
  new TransactionInstruction({
    programId: PROGRAM_ID,
    keys: [
      meta(payer, true, true),
      meta(l.terms, false, true),
      meta(l.meter, false, false),
      meta(mint, false, false),
      meta(l.vault, false, true),
      meta(renterAta, false, true),
      meta(TOKEN_PROGRAM_ID, false, false),
    ],
    data: disc("claim_renter_refund"),
  });

// ------------------------------------------------- 1. escrow and authority

console.log("1. open_lease escrows the window and records who may meter it");
const a = lease(crypto.randomBytes(16));
await send(openIx(a, 100n, 600n), [renter]);
assertEq("vault escrow", (await getAccount(conn, a.vault, "confirmed")).amount, 60_000n);
const openedA = decodeTerms((await conn.getAccountInfo(a.terms, "confirmed")).data);
assertEq("coordinator recorded", openedA.coordinator.toBase58(), coordinator.publicKey.toBase58());
assertEq("validator recorded", openedA.erValidator.toBase58(), validator.toBase58());
assertEq("funded is what the vault received", openedA.funded, 60_000n);

console.log("\n2. a job id alone does not get you the lease address");
const squatterTerms = pda([Buffer.from("lease"), stranger.publicKey.toBuffer(), a.jobId]);
assertEq("a different renter derives a different lease", squatterTerms.equals(a.terms), false);

console.log("\n3. who may drive the meter");
const bigTick = (l, signer) => tickIx(l, 600_000_000n, crypto.randomBytes(32), signer);
await refuses("a tick with no signer at all", ERR_NOT_ENOUGH_KEYS, bigTick(a, null), [renter]);
await refuses("a tick signed by a stranger", ERR_HAS_ONE, bigTick(a, stranger.publicKey), [renter, stranger]);
await send(tickIx(a, 5_000n, crypto.randomBytes(32), coordinator.publicKey), [renter, coordinator]);
assertEq("the coordinator's tick lands", decodeMeter((await conn.getAccountInfo(a.meter, "confirmed")).data).meteredMs, 5_000n);
await refuses(
  "a meter that would move backwards",
  ERR_METER_WENT_BACKWARDS,
  tickIx(a, 4_000n, crypto.randomBytes(32), coordinator.publicKey),
  [renter, coordinator],
);

console.log("\n4. settlement is closed while the lease is still running");
await refuses("settle_lease before the window is over", ERR_LEASE_STILL_RUNNING, settleIx(a, stranger.publicKey), [
  stranger,
]);
await refuses("claim_operator_share before the window is over", ERR_LEASE_STILL_RUNNING, claimOperatorIx(a, stranger.publicKey), [
  stranger,
]);
await refuses("claim_renter_refund before the window is over", ERR_LEASE_STILL_RUNNING, claimRenterIx(a, stranger.publicKey), [
  stranger,
]);

console.log("\n5. void_lease cancels the charge, and only the coordinator can");
await refuses("void_lease signed by a stranger", ERR_HAS_ONE, voidIx(a, stranger.publicKey), [stranger]);
await send(voidIx(a, coordinator.publicKey), [coordinator]);
assertEq("lease voided", decodeTerms((await conn.getAccountInfo(a.terms, "confirmed")).data).voided, true);
await refuses("a second void", ERR_ALREADY_VOIDED, voidIx(a, coordinator.publicKey), [coordinator]);

const renterBeforeA = (await getAccount(conn, renterAta, "confirmed")).amount;
const operatorBeforeA = (await getAccount(conn, operatorAta, "confirmed")).amount;
const renterLamportsBeforeA = await conn.getBalance(renter.publicKey, "confirmed");
await send(settleIx(a, stranger.publicKey), [stranger]);
assertEq(
  "a voided lease refunds the whole escrow however long it ran",
  (await getAccount(conn, renterAta, "confirmed")).amount - renterBeforeA,
  60_000n,
);
assertEq(
  "a voided lease pays the operator nothing",
  (await getAccount(conn, operatorAta, "confirmed")).amount - operatorBeforeA,
  0n,
);
assertEq("the vault is closed", (await conn.getAccountInfo(a.vault, "confirmed")) === null, true);
assertEq(
  "the vault's rent went back to the renter",
  (await conn.getBalance(renter.publicKey, "confirmed")) > renterLamportsBeforeA,
  true,
);

// ---------------------------------------- 6. the metered path, past the window

console.log("\n6. once the window elapses, anyone may settle on the committed meter");
const b = lease(crypto.randomBytes(16));
await send(openIx(b, 100n, 3n), [renter]); // 300 micro escrowed over 3s
await send(tickIx(b, 2_500n, crypto.randomBytes(32), coordinator.publicKey), [renter, coordinator]);
await refuses("settle inside the window", ERR_LEASE_STILL_RUNNING, settleIx(b, stranger.publicKey), [stranger]);
await sleep(4_000);
const renterBeforeB = (await getAccount(conn, renterAta, "confirmed")).amount;
const operatorBeforeB = (await getAccount(conn, operatorAta, "confirmed")).amount;
await send(settleIx(b, stranger.publicKey), [stranger]);
// ceil(100 * 2500 / 1000) = 250 of the 300 escrowed.
assertEq(
  "the operator is paid what the meter says",
  (await getAccount(conn, operatorAta, "confirmed")).amount - operatorBeforeB,
  250n,
);
assertEq(
  "the renter keeps the rest",
  (await getAccount(conn, renterAta, "confirmed")).amount - renterBeforeB,
  50n,
);

console.log("\n7. an inflated meter cannot charge past the escrow");
const c = lease(crypto.randomBytes(16));
await send(openIx(c, 100n, 3n), [renter]);
await send(tickIx(c, 18_446_744_073_709_551_615n, crypto.randomBytes(32), coordinator.publicKey), [renter, coordinator]);
await sleep(4_000);
const operatorBeforeC = (await getAccount(conn, operatorAta, "confirmed")).amount;
await send(settleIx(c, stranger.publicKey), [stranger]);
assertEq(
  "the charge clamps at the escrowed window",
  (await getAccount(conn, operatorAta, "confirmed")).amount - operatorBeforeC,
  300n,
);

// ------------------------------------------------- 8. the two payout legs

console.log("\n8. either side can be paid without the other");
const d = lease(crypto.randomBytes(16));
await send(openIx(d, 100n, 3n), [renter]);
await send(tickIx(d, 1_000n, crypto.randomBytes(32), coordinator.publicKey), [renter, coordinator]);
await sleep(4_000);
const renterBeforeD = (await getAccount(conn, renterAta, "confirmed")).amount;
const operatorBeforeD = (await getAccount(conn, operatorAta, "confirmed")).amount;
await send(claimRenterIx(d, stranger.publicKey), [stranger]);
assertEq(
  "the renter's refund leaves the operator's share behind",
  (await getAccount(conn, renterAta, "confirmed")).amount - renterBeforeD,
  200n,
);
assertEq("the operator's share is still in the vault", (await getAccount(conn, d.vault, "confirmed")).amount, 100n);
await refuses("a second renter refund", ERR_ALREADY_PAID, claimRenterIx(d, stranger.publicKey), [stranger]);
await send(claimOperatorIx(d, stranger.publicKey), [stranger]);
assertEq(
  "the operator claims theirs afterwards",
  (await getAccount(conn, operatorAta, "confirmed")).amount - operatorBeforeD,
  100n,
);
await refuses("a second operator claim", ERR_ALREADY_PAID, claimOperatorIx(d, stranger.publicKey), [stranger]);

// --------------------------------- 9. a lease whose meter never ran at all

console.log("\n9. a lease that never metered pays nobody and refunds everything");
const e = lease(crypto.randomBytes(16));
await send(openIx(e, 100n, 3n), [renter]);
await sleep(4_000);
const renterBeforeE = (await getAccount(conn, renterAta, "confirmed")).amount;
const operatorBeforeE = (await getAccount(conn, operatorAta, "confirmed")).amount;
await send(settleIx(e, stranger.publicKey), [stranger]);
assertEq(
  "the renter is made whole",
  (await getAccount(conn, renterAta, "confirmed")).amount - renterBeforeE,
  300n,
);
assertEq(
  "the operator is paid nothing",
  (await getAccount(conn, operatorAta, "confirmed")).amount - operatorBeforeE,
  0n,
);

console.log(`\n${checks} checks passed against ${RPC}`);
