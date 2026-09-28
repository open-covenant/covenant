#!/usr/bin/env node
// Drives the Covenant Compute lease meter end to end on Solana devnet:
// open_lease and delegate_lease on L1, N gasless ticks inside a MagicBlock
// Ephemeral Rollup, then undelegate, reconcile and settle back on L1.
//
// The point of the run is the reconciliation at step 7. The meter advances
// inside the rollup, where a tick is free and sub-second, and the amount that
// lands on L1 has to equal what the ticks say it should — including the
// provenance hash-chain, which is recomputed here from the receipts the client
// sent and compared byte for byte against committed state.
//
// Four of the steps are refusals rather than successes. A meter that anyone
// can drive is worth nothing, so the run also proves that an unsigned tick, a
// tick signed by the wrong key, an early settle and a tick after the meter
// closed are all rejected on chain.
//
// The escrow token is a throwaway 6-decimal SPL mint created by this script.
// It stands in for USDC because devnet USDC is not freely mintable. Every
// "micro-USDC" figure below is denominated in that stand-in mint, not in USDC.
//
// Usage: node lease-er.mjs            (defaults: 30 ticks, EU devnet validator)
//        N=100 ER=https://devnet-as.magicblock.app node lease-er.mjs
//        N=60 TICK_INTERVAL_MS=1000 node lease-er.mjs   (a minute in real time)
//        TARGET=settlement PROGRAM=<id> node lease-er.mjs
//            drives the same lifecycle against the lease meter inside the
//            settlement program (see lease-dialect.mjs)

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

// ---------------------------------------------------------------- constants

const DIALECT = resolveDialect();
const PROGRAM_ID = DIALECT.programId;
// Fixed MagicBlock ids. Magic11../MagicContext11.. exist only inside a rollup,
// which is why tick and undelegate are sent to the ER endpoint and never to L1.
const DELEGATION_PROGRAM_ID = new PublicKey("DELeGGvXpWV2fqJUhqcF5ZSYMS4JTLjteaAMARRSaeSh");
const MAGIC_PROGRAM_ID = new PublicKey("Magic11111111111111111111111111111111111111");
const MAGIC_CONTEXT_ID = new PublicKey("MagicContext1111111111111111111111111111111");

const L1_URL = process.env.RPC || "https://api.devnet.solana.com";
const ER_URL = process.env.ER || "https://devnet-eu.magicblock.app";
const VALIDATOR = new PublicKey(
  process.env.VALIDATOR || "MEUGGrYPxKk17hCr7wpT6s8dtNokZj5U2L57vjYMS8e",
);

const TICKS = Number.parseInt(process.env.N || "30", 10);
const MS_PER_TICK = BigInt(process.env.MS_PER_TICK || "1000");
// Wall-clock spacing between ticks. Zero sends them back to back, which
// measures the rollup; setting it to MS_PER_TICK makes the run a real-time
// session, where the meter and the clock advance together.
const TICK_INTERVAL_MS = Number.parseInt(process.env.TICK_INTERVAL_MS || "0", 10);
const RATE = BigInt(process.env.RATE || "100"); // micro-units per second
const MAX_DURATION = BigInt(process.env.MAX_DURATION || "600"); // seconds
const MINT_DECIMALS = 6;
const RENTER_SUPPLY = 1_000_000n; // 1.0 of the stand-in mint

// Anchor error codes this run expects to see, straight out of the program.
const ERR_HAS_ONE = DIALECT.errors.wrongCoordinator;
const ERR_NOT_ENOUGH_KEYS = DIALECT.errors.notEnoughKeys;
const ERR_METER_CLOSED = DIALECT.errors.meterClosed;
const ERR_LEASE_STILL_RUNNING = DIALECT.errors.leaseStillRunning;

const HERE = path.dirname(fileURLToPath(import.meta.url));
const REPO = path.resolve(HERE, "..", "..", "..");
const KEYDIR = path.join(REPO, "scratchpad", "compute-mainnet");
const RESULT = path.join(KEYDIR, DIALECT.leaseResultFile);

// ------------------------------------------------------------------ helpers

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const disc = (name) => crypto.createHash("sha256").update(`global:${name}`).digest().subarray(0, 8);
const sha256 = (...parts) => crypto.createHash("sha256").update(Buffer.concat(parts)).digest();
const meta = (pubkey, isSigner, isWritable) => ({ pubkey, isSigner, isWritable });
const pda = (seeds, program) => PublicKey.findProgramAddressSync(seeds, program)[0];
const sol = (lamports) => `${(lamports / LAMPORTS_PER_SOL).toFixed(9)} SOL`;
const txLink = (sig) => `https://explorer.solana.com/tx/${sig}?cluster=devnet`;
const acctLink = (a) => `https://explorer.solana.com/address/${a}?cluster=devnet`;

function fail(message) {
  throw new Error(message);
}

function assertEq(label, actual, expected) {
  const a = typeof actual === "bigint" ? actual.toString() : String(actual);
  const e = typeof expected === "bigint" ? expected.toString() : String(expected);
  if (a !== e) fail(`${label}: got ${a}, expected ${e}`);
  console.log(`  ok  ${label} = ${a}`);
}

// The Anchor error code behind a failed send, wherever the RPC put it: the
// message, the simulation logs, or the structured InstructionError.
function customErrorCode(error) {
  const hay = [error?.message ?? "", ...(error?.logs ?? [])].join("\n");
  const hex = hay.match(/custom program error: 0x([0-9a-fA-F]+)/);
  if (hex) return Number.parseInt(hex[1], 16);
  const custom = error?.transactionError?.err?.InstructionError?.[1]?.Custom;
  return typeof custom === "number" ? custom : null;
}

async function refuses(label, expectedCode, send) {
  try {
    await send();
  } catch (e) {
    const code = customErrorCode(e);
    if (code !== expectedCode) {
      fail(`${label}: rejected with ${code ?? e.message}, expected error ${expectedCode}`);
    }
    console.log(`  ok  ${label} (rejected with ${expectedCode})`);
    return;
  }
  fail(`${label}: the transaction was accepted`);
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

function loadKeyIfPresent(name) {
  const file = path.join(KEYDIR, `${name}.json`);
  if (!fs.existsSync(file)) return null;
  return Keypair.fromSecretKey(new Uint8Array(JSON.parse(fs.readFileSync(file, "utf8"))));
}

// A public devnet RPC drops a blockhash or falls behind often enough to end a
// sixty-tick run on the weather. Retry those, and only those: a transaction the
// program refused carries a custom error code, and the refusals below are the
// point of the run.
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

// LeaseTerms, as laid out by Anchor after the 8-byte account discriminator.
// Never delegated, so nothing here can be rewritten by the rollup host.
function decodeTerms(data) {
  return {
    jobId: Buffer.from(data.subarray(8, 24)),
    renter: new PublicKey(data.subarray(24, 56)),
    operator: new PublicKey(data.subarray(56, 88)),
    coordinator: new PublicKey(data.subarray(88, 120)),
    erValidator: new PublicKey(data.subarray(120, 152)),
    mint: new PublicKey(data.subarray(152, 184)),
    rate: data.readBigUInt64LE(184),
    maxDuration: data.readBigUInt64LE(192),
    funded: data.readBigUInt64LE(200),
    openedAt: data.readBigInt64LE(208),
    paidOperator: data[216] === 1,
    paidRenter: data[217] === 1,
    voided: data[218] === 1,
    delegated: data[219] === 1,
    bump: data[220],
    meterBump: data[221],
    vaultBump: data[222],
  };
}

// LeaseMeter: the delegated half. Elapsed time and a hash chain, nothing that
// decides money.
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

/// Ensure `who` holds at least `lamports` on devnet. The faucet is the first
/// choice; when it is rate-limited the local devnet deployer tops the account
/// up instead, so a run is never blocked on faucet weather.
async function ensureFunded(l1, who, lamports, label) {
  const have = await l1.getBalance(who.publicKey, "confirmed");
  if (have >= lamports) {
    console.log(`  ${label} funded: ${sol(have)}`);
    return { source: "existing", lamports: have };
  }
  const need = lamports - have;
  try {
    const sig = await l1.requestAirdrop(who.publicKey, need);
    await l1.confirmTransaction(sig, "confirmed");
    console.log(`  ${label} airdropped ${sol(need)}  ${sig}`);
    return { source: "faucet", lamports: await l1.getBalance(who.publicKey, "confirmed") };
  } catch (e) {
    console.log(`  ${label} airdrop unavailable (${e.message.split("\n")[0]}), using local devnet funder`);
  }
  const funder = loadKeyIfPresent("devnet-deployer");
  if (!funder) fail(`${label} needs ${sol(need)} and neither the faucet nor a local funder is available`);
  const sig = await send(
    l1,
    SystemProgram.transfer({ fromPubkey: funder.publicKey, toPubkey: who.publicKey, lamports: need }),
    [funder],
  );
  console.log(`  ${label} funded ${sol(need)} from local devnet funder  ${sig}`);
  return { source: "local-funder", lamports: await l1.getBalance(who.publicKey, "confirmed"), sig };
}

// --------------------------------------------------------------------- main

const l1 = new Connection(L1_URL, "confirmed");
const er = new Connection(ER_URL, "confirmed");

console.log("Covenant Compute: onchain lease meter on a MagicBlock Ephemeral Rollup (devnet)\n");
console.log(`  program    ${PROGRAM_ID.toBase58()}  (${DIALECT.name})`);
console.log(`  L1         ${L1_URL}`);
console.log(`  ER         ${ER_URL}`);
console.log(`  validator  ${VALIDATOR.toBase58()}`);

const erIdentity = await er
  ._rpcRequest("getIdentity", [])
  .then((r) => r.result?.identity)
  .catch(() => null);
if (erIdentity !== VALIDATOR.toBase58()) {
  fail(`ER endpoint ${ER_URL} reports identity ${erIdentity}, not the pinned validator ${VALIDATOR.toBase58()}`);
}
console.log(`  ER identity matches the pinned validator`);

const l1Program = await l1.getAccountInfo(PROGRAM_ID, "confirmed");
if (!l1Program) fail(`${PROGRAM_ID.toBase58()} is not deployed on ${L1_URL}`);

// The rollup executes its own clone of the program and refreshes it on its own
// schedule, so a run right after an upgrade can be judging the previous binary.
// Compare the bytes: the upgradeable loader keeps the ELF behind a 45-byte
// header on L1, the rollup's loader behind a 48-byte one. A rollup that has
// never seen this program has nothing to compare yet and clones it on first
// use, so the same check runs again once the meter is picked up, which is the
// point at which a stale clone would matter.
const programData = new PublicKey(l1Program.data.subarray(4, 36));
const l1Data = await l1.getAccountInfo(programData, "confirmed");
if (!l1Data) fail(`program data account ${programData.toBase58()} is missing on L1`);
const l1Elf = l1Data.data.subarray(45);
const l1Hash = crypto.createHash("sha256").update(l1Elf).digest("hex");

async function compareErClone(required) {
  const erData = await er.getAccountInfo(PROGRAM_ID, "confirmed").catch(() => null);
  if (!erData) {
    if (required) fail(`the ER at ${ER_URL} has no clone of ${PROGRAM_ID.toBase58()}`);
    console.log(`  ER binary  not cloned yet; the rollup fetches it on first use`);
    return false;
  }
  const erElf = erData.data.subarray(48);
  const erHash = crypto.createHash("sha256").update(erElf).digest("hex");
  console.log(`  ER binary  ${erElf.length} bytes  sha256 ${erHash}`);
  if (l1Hash !== erHash) {
    fail(
      `the ER is serving a stale clone of the program. Wait for it to refresh and re-run; ` +
        `undelegate would fail with ExternalAccountDataModified against a mismatched binary.`,
    );
  }
  console.log(`  ER is serving the current binary`);
  return true;
}

console.log(`  L1 binary  ${l1Elf.length} bytes  sha256 ${l1Hash}`);
await compareErClone(false);
console.log("");

// 1. Throwaway parties ------------------------------------------------------
console.log("1. renter, operator, coordinator");
const renter = loadOrCreateKey("devnet-renter");
const operator = loadOrCreateKey("devnet-operator");
const coordinator = loadOrCreateKey("devnet-coordinator");
const stranger = loadOrCreateKey("devnet-stranger");
console.log(`  renter      ${renter.publicKey.toBase58()}`);
console.log(`  operator    ${operator.publicKey.toBase58()}`);
console.log(`  coordinator ${coordinator.publicKey.toBase58()}  (the only key that may meter)`);
const renterFunding = await ensureFunded(l1, renter, 0.12 * LAMPORTS_PER_SOL, "renter");
const operatorFunding = await ensureFunded(l1, operator, 0.01 * LAMPORTS_PER_SOL, "operator");
await ensureFunded(l1, coordinator, 0.02 * LAMPORTS_PER_SOL, "coordinator");

// 2. Stand-in escrow mint ----------------------------------------------------
console.log("\n2. escrow mint");
console.log("  NOTE: this is a throwaway 6-decimal SPL mint created by this run, not devnet USDC.");
console.log("        Devnet USDC is not freely mintable, so the amounts below are denominated");
console.log("        in the stand-in mint. The program treats any 6-decimal mint identically.");
const mint = await createMint(l1, renter, renter.publicKey, null, MINT_DECIMALS, undefined, {
  commitment: "confirmed",
});
const renterAta = await getOrCreateAssociatedTokenAccount(l1, renter, mint, renter.publicKey, false, "confirmed");
const operatorAta = await getOrCreateAssociatedTokenAccount(l1, renter, mint, operator.publicKey, false, "confirmed");
await mintTo(l1, renter, mint, renterAta.address, renter, RENTER_SUPPLY, [], { commitment: "confirmed" });
const renterStart = (await getAccount(l1, renterAta.address, "confirmed")).amount;
console.log(`  mint       ${mint.toBase58()} (${MINT_DECIMALS} decimals)`);
console.log(`  renter ATA ${renterAta.address.toBase58()}  balance ${renterStart}`);
console.log(`  oper.  ATA ${operatorAta.address.toBase58()}  balance 0`);

// 3. open_lease --------------------------------------------------------------
console.log("\n3. open_lease");
const jobId = process.env.JOB_ID ? Buffer.from(process.env.JOB_ID, "hex") : crypto.randomBytes(16);
if (jobId.length !== 16) fail("JOB_ID must be 16 hex-encoded bytes");
// The renter is in the lease seeds, so learning a job id is not enough to
// occupy the address the honest open derives.
const terms = pda([Buffer.from("lease"), renter.publicKey.toBuffer(), jobId], PROGRAM_ID);
const meter = pda([Buffer.from("meter"), terms.toBuffer()], PROGRAM_ID);
const vault = pda([Buffer.from("vault"), terms.toBuffer()], PROGRAM_ID);
const funded = RATE * MAX_DURATION;

// disc(8) || job_id(16) || rate(8) || max_duration(8)
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
      // The renter names who may meter them, and which rollup identity may
      // host that meter. Both are fixed for the life of the lease.
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
console.log(`  terms PDA  ${terms.toBase58()}`);
console.log(`  meter PDA  ${meter.toBase58()}`);
console.log(`  vault PDA  ${vault.toBase58()}`);
console.log(`  tx         ${openSig}`);
assertEq("vault escrow", (await getAccount(l1, vault, "confirmed")).amount, funded);
assertEq("renter debited", (await getAccount(l1, renterAta.address, "confirmed")).amount, renterStart - funded);
const openedTerms = decodeTerms((await l1.getAccountInfo(terms, "confirmed")).data);
assertEq("coordinator recorded on chain", openedTerms.coordinator.toBase58(), coordinator.publicKey.toBase58());
assertEq("validator recorded on chain", openedTerms.erValidator.toBase58(), VALIDATOR.toBase58());

const settleIx = (payer) =>
  new TransactionInstruction({
    programId: PROGRAM_ID,
    keys: [
      meta(payer, true, true),
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
  });

// 4. what the program refuses on L1 ------------------------------------------
console.log("\n4. refusals on L1, before the meter has run");
// Settling here would return the whole escrow at charge zero while the
// operator serves the session — the gap between open and delegate is a real
// slot boundary, so the window has to be closed in the program.
await refuses("settle before the lease is over", ERR_LEASE_STILL_RUNNING, () =>
  send(l1, settleIx(renter.publicKey), [renter]),
);

// 5. delegate_lease ----------------------------------------------------------
console.log("\n5. delegate_lease");
const delegateSig = await send(
  l1,
  new TransactionInstruction({
    programId: PROGRAM_ID,
    keys: [
      meta(renter.publicKey, true, true),
      // Handing a meter to a third-party host is the other path the protocol
      // pause gates in the settlement build.
      ...(DIALECT.config ? [meta(DIALECT.config, false, false)] : []),
      meta(terms, false, true),
      // Only the coordinator may hand the meter to a rollup, and only to the
      // validator recorded at open.
      meta(coordinator.publicKey, true, false),
      meta(VALIDATOR, false, false),
      meta(pda([Buffer.from("buffer"), meter.toBuffer()], PROGRAM_ID), false, true),
      meta(pda([Buffer.from("delegation"), meter.toBuffer()], DELEGATION_PROGRAM_ID), false, true),
      meta(pda([Buffer.from("delegation-metadata"), meter.toBuffer()], DELEGATION_PROGRAM_ID), false, true),
      meta(meter, false, true),
      meta(PROGRAM_ID, false, false),
      meta(DELEGATION_PROGRAM_ID, false, false),
      meta(SystemProgram.programId, false, false),
    ],
    data: disc("delegate_lease"),
  }),
  [renter, coordinator],
);
console.log(`  tx         ${delegateSig}`);
const delegatedOwner = (await l1.getAccountInfo(meter, "confirmed"))?.owner;
assertEq("L1 meter owner is the delegation program", delegatedOwner?.toBase58(), DELEGATION_PROGRAM_ID.toBase58());
const delegatedTerms = await l1.getAccountInfo(terms, "confirmed");
assertEq("the escrow stayed on L1 with the program", delegatedTerms?.owner.toBase58(), PROGRAM_ID.toBase58());
assertEq("the lease records that its meter left for the rollup", decodeTerms(delegatedTerms.data).delegated, true);

let pickupMs = null;
for (let i = 0; i < 30; i++) {
  await sleep(1000);
  const info = await er.getAccountInfo(meter, "confirmed").catch(() => null);
  if (info && info.owner.equals(PROGRAM_ID)) {
    pickupMs = (i + 1) * 1000;
    console.log(`  ER picked the meter up after ~${pickupMs}ms, owner ${info.owner.toBase58()}`);
    break;
  }
}
if (pickupMs === null) fail(`the ER at ${ER_URL} never picked up ${meter.toBase58()}`);
// Now that the rollup has the meter it also has the program, so the clone is
// checkable. Do it before any ticks are sent: a stale clone here would take
// the whole run down at the undelegate.
await compareErClone(true);

// 6. tick, inside the rollup -------------------------------------------------
const tickData = (meteredMs, receiptHash) => {
  const data = Buffer.alloc(48);
  disc(DIALECT.tick).copy(data, 0);
  data.writeBigUInt64LE(meteredMs, 8);
  receiptHash.copy(data, 16);
  return data;
};

console.log("\n6. who may drive the meter");
// The account is reachable by anyone who can reach the rollup and its address
// follows from the job id, so an unsigned tick is the whole attack: drive the
// meter to the full escrowed window, then settle.
await refuses("a tick with no signer at all", ERR_NOT_ENOUGH_KEYS, () =>
  send(
    er,
    new TransactionInstruction({
      programId: PROGRAM_ID,
      keys: [meta(meter, false, true)],
      data: tickData(MS_PER_TICK * BigInt(TICKS) * 1000n, crypto.randomBytes(32)),
    }),
    [renter],
  ),
);
await refuses("a tick signed by a stranger", ERR_HAS_ONE, () =>
  send(
    er,
    new TransactionInstruction({
      programId: PROGRAM_ID,
      keys: [meta(meter, false, true), meta(stranger.publicKey, true, false)],
      data: tickData(MS_PER_TICK * BigInt(TICKS) * 1000n, crypto.randomBytes(32)),
    }),
    [renter, stranger],
  ),
);

console.log(`\n7. ${TICKS} coordinator ticks against the ER (gasless, cumulative meter)`);
const renterL1Before = await l1.getBalance(renter.publicKey, "confirmed");

const latencies = [];
const receipts = [];
let expectedRoot = Buffer.alloc(32); // PROVENANCE_GENESIS
let meteredMs = 0n;
let firstTickSig = null;
let lastTickSig = null;

const tickPhaseStart = Date.now();
for (let i = 0; i < TICKS; i++) {
  // Hold the cadence against a fixed origin so the send latency is absorbed
  // by the gap rather than added to it, the way a coordinator on a timer runs.
  if (TICK_INTERVAL_MS > 0) {
    const due = tickPhaseStart + i * TICK_INTERVAL_MS;
    const wait = due - Date.now();
    if (wait > 0) await sleep(wait);
  }
  meteredMs += MS_PER_TICK;
  const seq = Buffer.alloc(4);
  seq.writeUInt32BE(i);
  const receiptHash = sha256(jobId, seq, crypto.randomBytes(16));
  const t0 = Date.now();
  const sig = await send(
    er,
    new TransactionInstruction({
      programId: PROGRAM_ID,
      keys: [meta(meter, false, true), meta(coordinator.publicKey, true, false)],
      data: tickData(meteredMs, receiptHash),
    }),
    [renter, coordinator],
    { skipPreflight: true },
  );
  latencies.push(Date.now() - t0);
  receipts.push({ seq: i, meteredMs: meteredMs.toString(), receiptHash: receiptHash.toString("hex"), sig });
  expectedRoot = sha256(expectedRoot, receiptHash);
  if (!firstTickSig) firstTickSig = sig;
  lastTickSig = sig;
  if ((i + 1) % 10 === 0) console.log(`  ${i + 1}/${TICKS} ticks, last ${latencies.at(-1)}ms`);
}
const tickPhaseMs = Date.now() - tickPhaseStart;
if (TICK_INTERVAL_MS > 0) {
  console.log(`  cadence    one tick every ${TICK_INTERVAL_MS}ms, ${TICKS} ticks in ${tickPhaseMs}ms of wall clock`);
}

const renterL1After = await l1.getBalance(renter.publicKey, "confirmed");
const sorted = [...latencies].sort((a, b) => a - b);
const latency = {
  min: sorted[0],
  p50: sorted[Math.floor(sorted.length * 0.5)],
  p95: sorted[Math.min(sorted.length - 1, Math.floor(sorted.length * 0.95))],
  max: sorted.at(-1),
  avg: +(latencies.reduce((a, b) => a + b, 0) / latencies.length).toFixed(1),
};
console.log(`  latency    min ${latency.min}ms  p50 ${latency.p50}ms  p95 ${latency.p95}ms  max ${latency.max}ms`);
assertEq("renter L1 lamports unchanged by ticks", renterL1After, renterL1Before);

// The other half of "gasless": read every tick back out of the rollup and add
// up what it charged. The fee payer's rollup-side balance is not the right
// measurement here — the ER holds a lazily refreshed clone of an L1 account,
// so that number moves when the clone catches up, for reasons unrelated to
// fees. The fee recorded against each transaction is the actual answer.
let erFeesTotal = 0;
for (const r of receipts) {
  let tx = null;
  for (let attempt = 0; attempt < 10 && !tx; attempt++) {
    tx = await er.getTransaction(r.sig, { commitment: "confirmed", maxSupportedTransactionVersion: 0 });
    if (!tx) await sleep(300);
  }
  if (!tx) fail(`tick ${r.seq} (${r.sig}) never became readable from ${ER_URL}`);
  if (tx.meta?.err) fail(`tick ${r.seq} (${r.sig}) failed on the ER: ${JSON.stringify(tx.meta.err)}`);
  r.fee = tx.meta.fee;
  r.slot = tx.slot;
  erFeesTotal += tx.meta.fee;
}
assertEq(`total ER fee across ${TICKS} ticks (lamports)`, erFeesTotal, 0);

const erMeter = decodeMeter((await er.getAccountInfo(meter, "confirmed")).data);
// The charge is not computed in the rollup: the host that commits this account
// could rewrite anything in it, so the rate and the escrow stay on L1 and the
// split is derived there from the elapsed the meter committed.
const expectedCharged = (() => {
  const raw = (RATE * meteredMs + 999n) / 1000n; // div_ceil, matching the program
  return raw < funded ? raw : funded;
})();
assertEq("ER meter metered_ms", erMeter.meteredMs, meteredMs);
assertEq("ER meter concluded flag, still open mid-session", erMeter.concluded, false);

// 8. undelegate_lease and reconcile on L1 ------------------------------------
console.log("\n8. undelegate_lease, then reconcile the committed state on L1");
const undelegateSig = await send(
  er,
  new TransactionInstruction({
    programId: PROGRAM_ID,
    keys: [
      meta(renter.publicKey, true, true),
      meta(meter, false, true),
      meta(coordinator.publicKey, true, false),
      meta(MAGIC_PROGRAM_ID, false, false),
      meta(MAGIC_CONTEXT_ID, false, true),
    ],
    data: disc("undelegate_lease"),
  }),
  [renter, coordinator],
);
console.log(`  ER tx      ${undelegateSig}`);

let commitMs = null;
let l1Meter = null;
for (let i = 0; i < 60; i++) {
  await sleep(1000);
  const info = await l1.getAccountInfo(meter, "confirmed").catch(() => null);
  if (info && info.owner.equals(PROGRAM_ID)) {
    commitMs = (i + 1) * 1000;
    l1Meter = decodeMeter(info.data);
    break;
  }
}
if (!l1Meter) fail("the undelegate commit never landed on L1 within 60s");
console.log(`  commit landed on L1 after ~${commitMs}ms`);
assertEq("L1 metered_ms", l1Meter.meteredMs, meteredMs);
assertEq("L1 meter closed", l1Meter.concluded, true);
assertEq(
  "L1 provenance_root matches the client-side hash chain",
  l1Meter.provenanceRoot.toString("hex"),
  expectedRoot.toString("hex"),
);

// A meter that lands on L1 is writable again, and the coordinator is the one
// key that could raise it after the renter has already reconciled the number.
// The commit carries the closed flag, so it cannot.
await refuses("a tick on a closed meter", ERR_METER_CLOSED, () =>
  send(
    l1,
    new TransactionInstruction({
      programId: PROGRAM_ID,
      keys: [meta(meter, false, true), meta(coordinator.publicKey, true, false)],
      data: tickData(meteredMs * 1000n, crypto.randomBytes(32)),
    }),
    [coordinator],
  ),
);

// 9. settle_lease ------------------------------------------------------------
console.log("\n9. settle_lease");
const refund = funded - expectedCharged;
const renterLamportsBeforeSettle = await l1.getBalance(renter.publicKey, "confirmed");
const settleSig = await send(l1, settleIx(renter.publicKey), [renter]);
console.log(`  tx         ${settleSig}`);
const operatorFinal = (await getAccount(l1, operatorAta.address, "confirmed")).amount;
const renterFinal = (await getAccount(l1, renterAta.address, "confirmed")).amount;
const settledTerms = decodeTerms((await l1.getAccountInfo(terms, "confirmed")).data);
assertEq("operator paid exactly what the meter charged", operatorFinal, expectedCharged);
assertEq("renter refunded the remainder", renterFinal, renterStart - expectedCharged);
assertEq("charged + refunded == funded", expectedCharged + refund, funded);
assertEq("vault closed", (await l1.getAccountInfo(vault, "confirmed")) === null, true);
assertEq("operator share marked paid", settledTerms.paidOperator, true);
assertEq("renter refund marked paid", settledTerms.paidRenter, true);
const vaultRentReturned = (await l1.getBalance(renter.publicKey, "confirmed")) > renterLamportsBeforeSettle;
assertEq("the vault's rent went back to the renter", vaultRentReturned, true);

// ------------------------------------------------------------------- receipt

const receipt = {
  product: "Covenant Compute",
  ranAt: new Date().toISOString(),
  cluster: "devnet",
  target: DIALECT.name,
  program: PROGRAM_ID.toBase58(),
  l1Rpc: L1_URL,
  erRpc: ER_URL,
  validator: VALIDATOR.toBase58(),
  escrowMint: {
    address: mint.toBase58(),
    decimals: MINT_DECIMALS,
    note: "throwaway stand-in for USDC; devnet USDC is not freely mintable",
  },
  jobId: jobId.toString("hex"),
  terms: terms.toBase58(),
  meter: meter.toBase58(),
  vault: vault.toBase58(),
  renter: renter.publicKey.toBase58(),
  operator: operator.publicKey.toBase58(),
  coordinator: coordinator.publicKey.toBase58(),
  funding: { renter: renterFunding.source, operator: operatorFunding.source },
  refused: [
    "settle_lease before the meter concluded or the window elapsed",
    "tick with no signer",
    "tick signed by a key that is not the recorded coordinator",
    "tick on a meter the undelegate already closed",
  ],
  meter_run: {
    rateMicroPerSec: RATE.toString(),
    maxDurationSecs: MAX_DURATION.toString(),
    ticks: TICKS,
    tickIntervalMs: TICK_INTERVAL_MS,
    tickPhaseWallClockMs: tickPhaseMs,
    meteredMs: meteredMs.toString(),
    fundedMicro: funded.toString(),
    chargedMicro: expectedCharged.toString(),
    refundedMicro: refund.toString(),
    provenanceRoot: expectedRoot.toString("hex"),
  },
  erPickupMs: pickupMs,
  commitLandedMs: commitMs,
  latencyMs: latency,
  lamports: {
    renterL1BeforeTicks: renterL1Before,
    renterL1AfterTicks: renterL1After,
    l1SpentOnTicks: renterL1Before - renterL1After,
    erFeesOnTicks: erFeesTotal,
  },
  signatures: {
    openLease: openSig,
    delegateLease: delegateSig,
    firstTick: firstTickSig,
    lastTick: lastTickSig,
    undelegateLease: undelegateSig,
    settleLease: settleSig,
  },
  receipts,
};
fs.writeFileSync(RESULT, JSON.stringify(receipt, null, 2));

const line = "─".repeat(72);
console.log(`\n${line}`);
console.log("COVENANT COMPUTE LEASE METER RECEIPT (devnet)");
console.log(line);
console.log(`job id            ${jobId.toString("hex")}`);
console.log(`lease terms       ${terms.toBase58()}`);
console.log(`meter             ${meter.toBase58()}`);
console.log(`escrow mint       ${mint.toBase58()}  (stand-in for USDC, 6 decimals)`);
console.log(`coordinator       ${coordinator.publicKey.toBase58()}`);
console.log(`validator         ${VALIDATOR.toBase58()} @ ${ER_URL}`);
console.log(`ticks             ${TICKS} in the rollup, ${meteredMs}ms metered`);
console.log(`tick latency      min ${latency.min}ms  p50 ${latency.p50}ms  p95 ${latency.p95}ms  max ${latency.max}ms`);
console.log(
  `tick cost         ${erFeesTotal} lamports of rollup fees, ${renterL1Before - renterL1After} lamports off the renter's L1 balance`,
);
console.log(`ER pickup         ~${pickupMs}ms      commit to L1  ~${commitMs}ms`);
console.log(`funded            ${funded} micro`);
console.log(`charged           ${expectedCharged} micro  -> operator`);
console.log(`refunded          ${refund} micro  -> renter`);
console.log(`provenance root   ${expectedRoot.toString("hex")}`);
console.log(`                  recomputed from ${TICKS} tick receipts and equal to committed L1 state`);
console.log(`refused           unsigned tick, stranger tick, early settle, tick after close`);
console.log(line);
console.log(`open_lease        ${txLink(openSig)}`);
console.log(`delegate_lease    ${txLink(delegateSig)}`);
console.log(`settle_lease      ${txLink(settleSig)}`);
console.log(`lease terms       ${acctLink(terms.toBase58())}`);
console.log(`operator ATA      ${acctLink(operatorAta.address.toBase58())}`);
console.log(`ER first tick     ${firstTickSig}`);
console.log(`ER last tick      ${lastTickSig}`);
console.log(`ER undelegate     ${undelegateSig}`);
console.log(`                  rollup signatures resolve on ${ER_URL}, not on the L1 explorer`);
console.log(line);
console.log(`\nreceipt written to scratchpad/compute-mainnet/${DIALECT.leaseResultFile}`);
