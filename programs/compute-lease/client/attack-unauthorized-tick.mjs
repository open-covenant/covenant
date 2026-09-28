#!/usr/bin/env node
// Regression test for the metering-authority hole, played from the attacker's
// side on devnet.
//
// The hole: `tick` used to take no signer. The meter's address followed from
// the job id, the job id is handed to the assigned operator at dispatch, and a
// rollup accepts a transaction from anyone. So a stranger could drive someone
// else's meter to the top of the escrowed window for free and let settlement
// pay it out.
//
// This script is that stranger. It opens an honest lease, then attacks the
// meter with nothing but public information — the job id, the renter's address
// and the program id — on L1 and again inside the rollup, where the ticks are
// free. Every attempt has to be refused on chain, the honest coordinator has
// to still be able to meter in the same run, and settlement has to pay the
// operator the metered charge rather than the ceiling the attacker asked for.
//
// The attacks are sent with preflight disabled on purpose. A refusal that only
// ever happens in a simulator proves nothing, so each one is landed as a real
// transaction and its recorded on-chain error is what the run asserts.
//
// Usage: node attack-unauthorized-tick.mjs
//        RPC=<devnet rpc> ER=<rollup endpoint> node attack-unauthorized-tick.mjs
//        TARGET=settlement PROGRAM=<id> node attack-unauthorized-tick.mjs
//            replays the attack against the lease meter inside the settlement
//            program (see lease-dialect.mjs)

import fs from "node:fs";
import path from "node:path";
import crypto from "node:crypto";
import { fileURLToPath } from "node:url";
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
import { resolveDialect } from "./lease-dialect.mjs";

const DIALECT = resolveDialect();
const PROGRAM_ID = DIALECT.programId;
const DELEGATION_PROGRAM_ID = new PublicKey("DELeGGvXpWV2fqJUhqcF5ZSYMS4JTLjteaAMARRSaeSh");
const MAGIC_PROGRAM_ID = new PublicKey("Magic11111111111111111111111111111111111111");
const MAGIC_CONTEXT_ID = new PublicKey("MagicContext1111111111111111111111111111111");

const L1_URL = process.env.RPC || "https://api.devnet.solana.com";
const ER_URL = process.env.ER || "https://devnet-eu.magicblock.app";
const VALIDATOR = new PublicKey(
  process.env.VALIDATOR || "MEUGGrYPxKk17hCr7wpT6s8dtNokZj5U2L57vjYMS8e",
);

const RATE = 100n; // micro-units per second
const MAX_DURATION = 600n; // seconds
const HONEST_TICKS = 5;
const MS_PER_TICK = 1000n;
const MINT_DECIMALS = 6;
const RENTER_SUPPLY = 1_000_000n;

// What the attacker asks for: the full window, plus a second attempt at the
// largest number the field can hold.
const CEILING_MS = MAX_DURATION * 1000n;
const ABSURD_MS = 18_446_744_073_709_551_615n; // u64::MAX

const ERR_HAS_ONE = DIALECT.errors.wrongCoordinator;
const ERR_NOT_ENOUGH_KEYS = DIALECT.errors.notEnoughKeys;
const ERR_NOT_SIGNER = DIALECT.errors.notSigner;

const HERE = path.dirname(fileURLToPath(import.meta.url));
const REPO = path.resolve(HERE, "..", "..", "..");
const KEYDIR = path.join(REPO, "scratchpad", "compute-mainnet");
const RESULT = path.join(KEYDIR, DIALECT.attackResultFile);

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const disc = (name) => crypto.createHash("sha256").update(`global:${name}`).digest().subarray(0, 8);
const sha256 = (...parts) => crypto.createHash("sha256").update(Buffer.concat(parts)).digest();
const meta = (pubkey, isSigner, isWritable) => ({ pubkey, isSigner, isWritable });
const pda = (seeds, program) => PublicKey.findProgramAddressSync(seeds, program)[0];
const sol = (lamports) => `${(lamports / LAMPORTS_PER_SOL).toFixed(9)} SOL`;

function fail(message) {
  throw new Error(message);
}

function assertEq(label, actual, expected) {
  const a = typeof actual === "bigint" ? actual.toString() : String(actual);
  const e = typeof expected === "bigint" ? expected.toString() : String(expected);
  if (a !== e) fail(`${label}: got ${a}, expected ${e}`);
  console.log(`  ok  ${label} = ${a}`);
}

function loadOrCreateKey(name) {
  fs.mkdirSync(KEYDIR, { recursive: true, mode: 0o700 });
  const file = path.join(KEYDIR, `${name}.json`);
  if (fs.existsSync(file)) {
    return Keypair.fromSecretKey(new Uint8Array(JSON.parse(fs.readFileSync(file, "utf8"))));
  }
  const kp = Keypair.generate();
  fs.writeFileSync(file, JSON.stringify(Array.from(kp.secretKey)), { mode: 0o600 });
  console.log(`  created ${name}.json (throwaway devnet key, gitignored)`);
  return kp;
}

// A public devnet RPC drops a blockhash or falls behind often enough that an
// otherwise green regression fails on the weather. Retry those, and only
// those: a transaction the program refused carries a custom error code, and
// re-sending it would bury the finding this test exists to report.
const TRANSIENT = /blockhash not found|node is behind|block height exceeded|timed out|502|503|429/i;

async function send(conn, ix, signers, opts = {}) {
  for (let attempt = 0; ; attempt++) {
    try {
      return await sendAndConfirmTransaction(conn, new Transaction().add(ix), signers, {
        commitment: "confirmed",
        ...opts,
      });
    } catch (e) {
      const text = [e?.message ?? "", ...(e?.logs ?? [])].join("\n");
      if (attempt >= 4 || /custom program error/i.test(text) || !TRANSIENT.test(text)) throw e;
      await sleep(1000 * (attempt + 1));
    }
  }
}

// Land a transaction that is expected to fail and read the error the cluster
// recorded against it, so the refusal has a signature someone else can look up
// rather than a simulator's opinion.
async function landsAndFails(conn, label, expectedCode, ix, signers, feePayer) {
  const tx = new Transaction().add(ix);
  tx.feePayer = feePayer;
  tx.recentBlockhash = (await conn.getLatestBlockhash("confirmed")).blockhash;
  tx.sign(...signers);
  const signature = await conn.sendRawTransaction(tx.serialize(), { skipPreflight: true });

  let status = null;
  for (let i = 0; i < 60 && !status?.confirmationStatus; i++) {
    await sleep(500);
    status = (await conn.getSignatureStatus(signature, { searchTransactionHistory: true })).value;
  }
  if (!status) fail(`${label}: ${signature} never confirmed`);
  const code = status.err?.InstructionError?.[1]?.Custom ?? null;
  if (code !== expectedCode) {
    fail(`${label}: recorded ${JSON.stringify(status.err)}, expected custom error ${expectedCode}`);
  }
  console.log(`  ok  ${label}`);
  console.log(`      rejected on chain with ${expectedCode}   ${signature}`);
  return { label, expectedCode, signature, err: status.err };
}

function decodeMeter(data) {
  return {
    terms: new PublicKey(data.subarray(8, 40)),
    coordinator: new PublicKey(data.subarray(40, 72)),
    jobId: Buffer.from(data.subarray(72, 88)),
    meteredMs: data.readBigUInt64LE(88),
    provenanceRoot: Buffer.from(data.subarray(96, 128)),
    concluded: data[128] === 1,
    bump: data[129],
  };
}

const l1 = new Connection(L1_URL, "confirmed");
const er = new Connection(ER_URL, "confirmed");

console.log("Covenant Compute: unauthorized-tick regression test (devnet)\n");
console.log(`  program    ${PROGRAM_ID.toBase58()}`);
console.log(`  ER         ${ER_URL}`);
if (!(await l1.getAccountInfo(PROGRAM_ID))) fail(`${PROGRAM_ID.toBase58()} is not deployed on L1`);

// 1. the honest lease -------------------------------------------------------
console.log("\n1. an honest lease, opened by a renter who has never heard of the attacker");
const renter = loadOrCreateKey("devnet-renter");
const operator = loadOrCreateKey("devnet-operator");
const coordinator = loadOrCreateKey("devnet-coordinator");
const attacker = loadOrCreateKey("devnet-attacker");
const funder = loadOrCreateKey("devnet-deployer");
console.log(`  renter      ${renter.publicKey.toBase58()}`);
console.log(`  coordinator ${coordinator.publicKey.toBase58()}  (the only key that may meter)`);
console.log(`  attacker    ${attacker.publicKey.toBase58()}  (holds no key of anyone's)`);

for (const [who, name, want] of [
  [renter, "renter", 0.06 * LAMPORTS_PER_SOL],
  [coordinator, "coordinator", 0.02 * LAMPORTS_PER_SOL],
  [attacker, "attacker", 0.02 * LAMPORTS_PER_SOL],
]) {
  const have = await l1.getBalance(who.publicKey, "confirmed");
  if (have >= want) continue;
  const sig = await send(
    l1,
    SystemProgram.transfer({
      fromPubkey: funder.publicKey,
      toPubkey: who.publicKey,
      lamports: want - have,
    }),
    [funder],
  );
  console.log(`  funded ${name} ${sol(want - have)}  ${sig}`);
}

const mint = await createMint(l1, renter, renter.publicKey, null, MINT_DECIMALS, undefined, {
  commitment: "confirmed",
});
const renterAta = await getOrCreateAssociatedTokenAccount(
  l1,
  renter,
  mint,
  renter.publicKey,
  false,
  "confirmed",
);
const operatorAta = await getOrCreateAssociatedTokenAccount(
  l1,
  renter,
  mint,
  operator.publicKey,
  false,
  "confirmed",
);
await mintTo(l1, renter, mint, renterAta.address, renter, RENTER_SUPPLY, [], {
  commitment: "confirmed",
});
const renterStart = (await getAccount(l1, renterAta.address, "confirmed")).amount;
const operatorStart = (await getAccount(l1, operatorAta.address, "confirmed")).amount;

const jobId = crypto.randomBytes(16);
const terms = pda([Buffer.from("lease"), renter.publicKey.toBuffer(), jobId], PROGRAM_ID);
const meter = pda([Buffer.from("meter"), terms.toBuffer()], PROGRAM_ID);
const vault = pda([Buffer.from("vault"), terms.toBuffer()], PROGRAM_ID);
const funded = RATE * MAX_DURATION;

const openBuf = Buffer.alloc(40);
disc("open_lease").copy(openBuf, 0);
jobId.copy(openBuf, 8);
openBuf.writeBigUInt64LE(RATE, 24);
openBuf.writeBigUInt64LE(MAX_DURATION, 32);
const openSig = await send(
  l1,
  new TransactionInstruction({
    programId: PROGRAM_ID,
    keys: [
      // The settlement build reads the protocol pause before taking new money.
      ...(DIALECT.config ? [meta(DIALECT.config, false, false)] : []),
      meta(renter.publicKey, true, true),
      meta(operator.publicKey, false, false),
      meta(coordinator.publicKey, false, false),
      meta(VALIDATOR, false, false),
      meta(terms, false, true),
      meta(meter, false, true),
      meta(mint, false, false),
      meta(vault, false, true),
      meta(renterAta.address, false, true),
      meta(TOKEN_PROGRAM_ID, false, false),
      meta(SystemProgram.programId, false, false),
    ],
    data: openBuf,
  }),
  [renter],
);
console.log(`  job id     ${jobId.toString("hex")}`);
console.log(`  meter PDA  ${meter.toBase58()}`);
console.log(`  tx         ${openSig}`);
assertEq("escrow at risk (micro)", (await getAccount(l1, vault, "confirmed")).amount, funded);

// 2. what the attacker can work out on their own ----------------------------
console.log("\n2. what the attacker can derive from public information alone");
// The job id travels to whoever is assigned the work, and the renter's address
// is on the open transaction. That is the whole starting position.
const derivedTerms = pda([Buffer.from("lease"), renter.publicKey.toBuffer(), jobId], PROGRAM_ID);
const derivedMeter = pda([Buffer.from("meter"), derivedTerms.toBuffer()], PROGRAM_ID);
assertEq("attacker derives the live meter address", derivedMeter.toBase58(), meter.toBase58());
// The address the old seeds gave out. It is nobody's account now, which is
// what stops a squatter occupying a victim's lease slot before they open it.
const jobIdOnlyPda = pda([Buffer.from("lease"), jobId], PROGRAM_ID);
assertEq(
  "the job-id-only address the old seeds produced holds nothing",
  (await l1.getAccountInfo(jobIdOnlyPda, "confirmed")) === null,
  true,
);

const tickData = (meteredMs, receiptHash) => {
  const data = Buffer.alloc(48);
  disc(DIALECT.tick).copy(data, 0);
  data.writeBigUInt64LE(meteredMs, 8);
  receiptHash.copy(data, 16);
  return data;
};
const tickIx = (ms, keys, receiptHash = crypto.randomBytes(32)) =>
  new TransactionInstruction({ programId: PROGRAM_ID, keys, data: tickData(ms, receiptHash) });
const attackTick = (ms, keys) => tickIx(ms, keys);

const refusals = [];

// 3. the attack on L1 --------------------------------------------------------
console.log("\n3. the attack on L1, before the meter is delegated");
refusals.push(
  await landsAndFails(
    l1,
    "tick to the top of the window with no coordinator account at all",
    ERR_NOT_ENOUGH_KEYS,
    attackTick(CEILING_MS, [meta(meter, false, true)]),
    [attacker],
    attacker.publicKey,
  ),
);
refusals.push(
  await landsAndFails(
    l1,
    "tick signed by the attacker, standing in for the coordinator",
    ERR_HAS_ONE,
    attackTick(CEILING_MS, [meta(meter, false, true), meta(attacker.publicKey, true, false)]),
    [attacker],
    attacker.publicKey,
  ),
);
refusals.push(
  await landsAndFails(
    l1,
    "tick to u64::MAX, signed by the attacker",
    ERR_HAS_ONE,
    attackTick(ABSURD_MS, [meta(meter, false, true), meta(attacker.publicKey, true, false)]),
    [attacker],
    attacker.publicKey,
  ),
);
// Naming the coordinator is free; signing as them is not. The account is
// passed unsigned, which is as close as the attacker can get on their own.
refusals.push(
  await landsAndFails(
    l1,
    "tick naming the real coordinator, passed unsigned",
    ERR_NOT_SIGNER,
    attackTick(CEILING_MS, [meta(meter, false, true), meta(coordinator.publicKey, false, false)]),
    [attacker],
    attacker.publicKey,
  ),
);
assertEq(
  "meter still reads zero after the L1 attacks",
  decodeMeter((await l1.getAccountInfo(meter, "confirmed")).data).meteredMs,
  0n,
);

// 4. delegate, then attack where the ticks are free --------------------------
console.log("\n4. the same attack inside the rollup, where a tick costs nothing");
const delegateSig = await send(
  l1,
  new TransactionInstruction({
    programId: PROGRAM_ID,
    keys: [
      meta(renter.publicKey, true, true),
      ...(DIALECT.config ? [meta(DIALECT.config, false, false)] : []),
      meta(terms, false, true),
      meta(coordinator.publicKey, true, false),
      meta(VALIDATOR, false, false),
      meta(pda([Buffer.from("buffer"), meter.toBuffer()], PROGRAM_ID), false, true),
      meta(pda([Buffer.from("delegation"), meter.toBuffer()], DELEGATION_PROGRAM_ID), false, true),
      meta(
        pda([Buffer.from("delegation-metadata"), meter.toBuffer()], DELEGATION_PROGRAM_ID),
        false,
        true,
      ),
      meta(meter, false, true),
      meta(PROGRAM_ID, false, false),
      meta(DELEGATION_PROGRAM_ID, false, false),
      meta(SystemProgram.programId, false, false),
    ],
    data: disc("delegate_lease"),
  }),
  [renter, coordinator],
);
console.log(`  delegate   ${delegateSig}`);

let pickedUp = false;
for (let i = 0; i < 30 && !pickedUp; i++) {
  await sleep(1000);
  const info = await er.getAccountInfo(meter, "confirmed").catch(() => null);
  pickedUp = Boolean(info && info.owner.equals(PROGRAM_ID));
}
if (!pickedUp) fail(`the ER at ${ER_URL} never picked up ${meter.toBase58()}`);
console.log(`  the rollup is holding the meter; anyone can now send it a transaction`);

refusals.push(
  await landsAndFails(
    er,
    "rollup tick to the top of the window with no coordinator account",
    ERR_NOT_ENOUGH_KEYS,
    attackTick(CEILING_MS, [meta(meter, false, true)]),
    [attacker],
    attacker.publicKey,
  ),
);
refusals.push(
  await landsAndFails(
    er,
    "rollup tick signed by the attacker",
    ERR_HAS_ONE,
    attackTick(CEILING_MS, [meta(meter, false, true), meta(attacker.publicKey, true, false)]),
    [attacker],
    attacker.publicKey,
  ),
);
refusals.push(
  await landsAndFails(
    er,
    "rollup tick to u64::MAX, signed by the attacker",
    ERR_HAS_ONE,
    attackTick(ABSURD_MS, [meta(meter, false, true), meta(attacker.publicKey, true, false)]),
    [attacker],
    attacker.publicKey,
  ),
);
// Ending the meter early is the other half of driving it: a stranger who can
// conclude a live lease can settle it on whatever it has metered so far.
refusals.push(
  await landsAndFails(
    er,
    "rollup undelegate signed by the attacker",
    ERR_HAS_ONE,
    new TransactionInstruction({
      programId: PROGRAM_ID,
      keys: [
        meta(attacker.publicKey, true, true),
        meta(meter, false, true),
        meta(attacker.publicKey, true, false),
        meta(MAGIC_PROGRAM_ID, false, false),
        meta(MAGIC_CONTEXT_ID, false, true),
      ],
      data: disc("undelegate_lease"),
    }),
    [attacker],
    attacker.publicKey,
  ),
);
assertEq(
  "meter still reads zero after the rollup attacks",
  decodeMeter((await er.getAccountInfo(meter, "confirmed")).data).meteredMs,
  0n,
);

// 5. the coordinator, in the same run ----------------------------------------
console.log(`\n5. the coordinator meters the same lease, ${HONEST_TICKS} ticks`);
let meteredMs = 0n;
let provenance = Buffer.alloc(32);
const honest = [];
for (let i = 0; i < HONEST_TICKS; i++) {
  meteredMs += MS_PER_TICK;
  const seq = Buffer.alloc(4);
  seq.writeUInt32BE(i);
  const receiptHash = sha256(jobId, seq, crypto.randomBytes(16));
  const sig = await send(
    er,
    tickIx(meteredMs, [meta(meter, false, true), meta(coordinator.publicKey, true, false)], receiptHash),
    [coordinator],
  ).catch((e) => fail(`the legitimate coordinator tick ${i} was refused: ${e.message}`));
  provenance = sha256(provenance, receiptHash);
  honest.push({ seq: i, meteredMs: meteredMs.toString(), receiptHash: receiptHash.toString("hex"), sig });
}
console.log(`  ${HONEST_TICKS} coordinator ticks accepted, last ${honest.at(-1).sig}`);
const erMeter = decodeMeter((await er.getAccountInfo(meter, "confirmed")).data);
assertEq("meter advanced by the coordinator's ticks only", erMeter.meteredMs, meteredMs);
assertEq("and not to the ceiling the attacker asked for", erMeter.meteredMs < CEILING_MS, true);
// The attacker's ticks carried receipt hashes of their own. None of them are
// in the chain, so the root still replays from the coordinator's receipts.
assertEq(
  "provenance root replays from the coordinator's receipts alone",
  erMeter.provenanceRoot.toString("hex"),
  provenance.toString("hex"),
);

// 6. conclude and settle -----------------------------------------------------
console.log("\n6. undelegate and settle: what the attacker actually cost the renter");
const undelegateSig = await send(
  er,
  new TransactionInstruction({
    programId: PROGRAM_ID,
    keys: [
      meta(coordinator.publicKey, true, true),
      meta(meter, false, true),
      meta(coordinator.publicKey, true, false),
      meta(MAGIC_PROGRAM_ID, false, false),
      meta(MAGIC_CONTEXT_ID, false, true),
    ],
    data: disc("undelegate_lease"),
  }),
  [coordinator],
);
console.log(`  undelegate ${undelegateSig}`);

let l1Meter = null;
for (let i = 0; i < 60 && !l1Meter; i++) {
  await sleep(1000);
  const info = await l1.getAccountInfo(meter, "confirmed").catch(() => null);
  if (info && info.owner.equals(PROGRAM_ID)) l1Meter = decodeMeter(info.data);
}
if (!l1Meter) fail("the undelegate commit never landed on L1 within 60s");
assertEq("committed metered_ms", l1Meter.meteredMs, meteredMs);

const expectedCharge = (() => {
  const raw = (RATE * meteredMs + 999n) / 1000n;
  return raw < funded ? raw : funded;
})();
const settleSig = await send(
  l1,
  new TransactionInstruction({
    programId: PROGRAM_ID,
    keys: [
      meta(renter.publicKey, true, true),
      meta(terms, false, true),
      meta(meter, false, false),
      meta(renter.publicKey, false, true),
      meta(mint, false, false),
      meta(vault, false, true),
      meta(operatorAta.address, false, true),
      meta(renterAta.address, false, true),
      meta(TOKEN_PROGRAM_ID, false, false),
    ],
    data: disc("settle_lease"),
  }),
  [renter],
);
console.log(`  settle     ${settleSig}`);
const operatorFinal = (await getAccount(l1, operatorAta.address, "confirmed")).amount;
const renterFinal = (await getAccount(l1, renterAta.address, "confirmed")).amount;
assertEq("operator paid the metered charge", operatorFinal - operatorStart, expectedCharge);
assertEq("renter refunded the rest", renterFinal, renterStart - expectedCharge);
assertEq("the attacker moved nothing", operatorFinal - operatorStart < funded, true);

const receipt = {
  product: "Covenant Compute",
  test: "unauthorized tick",
  ranAt: new Date().toISOString(),
  cluster: "devnet",
  target: DIALECT.name,
  program: PROGRAM_ID.toBase58(),
  erRpc: ER_URL,
  jobId: jobId.toString("hex"),
  terms: terms.toBase58(),
  meter: meter.toBase58(),
  attacker: attacker.publicKey.toBase58(),
  coordinator: coordinator.publicKey.toBase58(),
  escrowAtRisk: funded.toString(),
  askedFor: { ceilingMs: CEILING_MS.toString(), absurdMs: ABSURD_MS.toString() },
  refusals,
  honestTicks: honest,
  meteredMs: meteredMs.toString(),
  provenanceRoot: provenance.toString("hex"),
  chargedMicro: expectedCharge.toString(),
  refundedMicro: (funded - expectedCharge).toString(),
  signatures: { openLease: openSig, delegateLease: delegateSig, undelegateLease: undelegateSig, settleLease: settleSig },
};
fs.writeFileSync(RESULT, JSON.stringify(receipt, null, 2));

const line = "─".repeat(72);
console.log(`\n${line}`);
console.log("UNAUTHORIZED TICK: REFUSED");
console.log(line);
console.log(`job id            ${jobId.toString("hex")}`);
console.log(`meter             ${meter.toBase58()}`);
console.log(`escrow at risk    ${funded} micro`);
console.log(`attacker asked    ${CEILING_MS}ms, then ${ABSURD_MS}ms`);
console.log(`attacker got      ${meteredMs}ms metered by the coordinator, ${expectedCharge} micro charged`);
console.log(`refusals          ${refusals.length}, each one a landed transaction`);
for (const r of refusals) {
  console.log(`  ${r.expectedCode}  ${r.signature}`);
  console.log(`      ${r.label}`);
}
console.log(line);
console.log(`\nreceipt written to scratchpad/compute-mainnet/${DIALECT.attackResultFile}`);
