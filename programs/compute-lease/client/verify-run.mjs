#!/usr/bin/env node
// Independent check on a finished lease-er.mjs run.
//
// The run script asserts its own arithmetic, which is worth exactly as much as
// the script. This one starts from the receipt file and devnet, never from the
// run's log: it pulls the two lease accounts back out of L1, decodes them from
// raw bytes, replays the hash chain over the tick receipts, and only then
// compares. The provenance root is the claim that matters, so it is recomputed
// here rather than read.
//
// Usage: node verify-run.mjs [receipt.json]

import fs from "node:fs";
import path from "node:path";
import crypto from "node:crypto";
import { fileURLToPath } from "node:url";
import { Connection, PublicKey } from "@solana/web3.js";

const HERE = path.dirname(fileURLToPath(import.meta.url));
const REPO = path.resolve(HERE, "..", "..", "..");
const RESULT =
  process.argv[2] || path.join(REPO, "scratchpad", "compute-mainnet", "lease-er-result.json");

const L1_URL = process.env.RPC || "https://api.devnet.solana.com";

const run = JSON.parse(fs.readFileSync(RESULT, "utf8"));
const l1 = new Connection(L1_URL, "confirmed");

let failures = 0;
function check(label, actual, expected) {
  const a = typeof actual === "bigint" ? actual.toString() : String(actual);
  const e = typeof expected === "bigint" ? expected.toString() : String(expected);
  const ok = a === e;
  if (!ok) failures += 1;
  console.log(`  ${ok ? "ok  " : "FAIL"} ${label}\n         got      ${a}\n         expected ${e}`);
}

// Layouts taken from the program's own account structs, offsets counted from
// the 8-byte Anchor discriminator.
const terms = (d) => ({
  jobId: d.subarray(8, 24).toString("hex"),
  renter: new PublicKey(d.subarray(24, 56)).toBase58(),
  operator: new PublicKey(d.subarray(56, 88)).toBase58(),
  coordinator: new PublicKey(d.subarray(88, 120)).toBase58(),
  erValidator: new PublicKey(d.subarray(120, 152)).toBase58(),
  mint: new PublicKey(d.subarray(152, 184)).toBase58(),
  rate: d.readBigUInt64LE(184),
  maxDuration: d.readBigUInt64LE(192),
  funded: d.readBigUInt64LE(200),
  openedAt: d.readBigInt64LE(208),
  paidOperator: d[216] === 1,
  paidRenter: d[217] === 1,
  voided: d[218] === 1,
  delegated: d[219] === 1,
});

const meter = (d) => ({
  terms: new PublicKey(d.subarray(8, 40)).toBase58(),
  coordinator: new PublicKey(d.subarray(40, 72)).toBase58(),
  jobId: d.subarray(72, 88).toString("hex"),
  meteredMs: d.readBigUInt64LE(88),
  provenanceRoot: d.subarray(96, 128).toString("hex"),
  concluded: d[128] === 1,
});

console.log(`receipt   ${path.basename(RESULT)}  (${run.ranAt})`);
console.log(`L1        ${L1_URL}`);
console.log(`program   ${run.program}\n`);

const termsInfo = await l1.getAccountInfo(new PublicKey(run.terms), "confirmed");
const meterInfo = await l1.getAccountInfo(new PublicKey(run.meter), "confirmed");
if (!termsInfo) throw new Error(`LeaseTerms ${run.terms} is not on ${L1_URL}`);
if (!meterInfo) throw new Error(`LeaseMeter ${run.meter} is not on ${L1_URL}`);

console.log("raw account data, re-read from devnet L1");
console.log(`  LeaseTerms ${run.terms}`);
console.log(`    owner ${termsInfo.owner.toBase58()}  ${termsInfo.data.length} bytes  ${termsInfo.lamports} lamports`);
console.log(`    sha256 ${crypto.createHash("sha256").update(termsInfo.data).digest("hex")}`);
console.log(`  LeaseMeter ${run.meter}`);
console.log(`    owner ${meterInfo.owner.toBase58()}  ${meterInfo.data.length} bytes  ${meterInfo.lamports} lamports`);
console.log(`    sha256 ${crypto.createHash("sha256").update(meterInfo.data).digest("hex")}`);

const t = terms(termsInfo.data);
const m = meter(meterInfo.data);
console.log("\ndecoded LeaseTerms");
for (const [k, v] of Object.entries(t)) console.log(`    ${k.padEnd(14)} ${v}`);
console.log("decoded LeaseMeter");
for (const [k, v] of Object.entries(m)) console.log(`    ${k.padEnd(14)} ${v}`);

// Replay the chain: root = sha256(root || receipt_hash), genesis 32 zero bytes.
let root = Buffer.alloc(32);
let last = 0n;
for (const r of run.receipts) {
  const ms = BigInt(r.meteredMs);
  if (ms <= last) throw new Error(`receipt ${r.seq} does not advance the meter: ${last} -> ${ms}`);
  last = ms;
  root = crypto.createHash("sha256").update(Buffer.concat([root, Buffer.from(r.receiptHash, "hex")])).digest();
}

const rate = t.rate;
const chargedExpected = (() => {
  const raw = (rate * last + 999n) / 1000n;
  return raw < t.funded ? raw : t.funded;
})();

console.log(`\nreplayed ${run.receipts.length} tick receipts`);
console.log(`  root ${root.toString("hex")}`);
console.log("\nchecks against chain state, not against the run's log");
check("provenance root on L1 equals the replayed chain", m.provenanceRoot, root.toString("hex"));
check("metered_ms on L1 equals the last receipt", m.meteredMs, last);
check("tick count equals metered_ms / ms-per-tick", run.receipts.length, Number(last / 1000n));
check("meter is concluded", m.concluded, true);
check("meter is bound to these terms", m.terms, run.terms);
check("meter carries the coordinator the renter named", m.coordinator, t.coordinator);
check("meter job id equals terms job id", m.jobId, t.jobId);
check("lease was delegated at least once", t.delegated, true);
check("operator paid", t.paidOperator, true);
check("renter refunded", t.paidRenter, true);
check("lease not voided", t.voided, false);
check("charged, recomputed from the on-chain rate", run.meter_run.chargedMicro, chargedExpected);
check("refunded == funded - charged", run.meter_run.refundedMicro, (t.funded - chargedExpected).toString());
check("funded on L1 equals the receipt", run.meter_run.fundedMicro, t.funded);

const vault = await l1.getAccountInfo(new PublicKey(run.vault), "confirmed");
check("vault account is closed", vault === null, true);

const fees = run.receipts.map((r) => r.fee);
const uniqueFees = [...new Set(fees)];
console.log(`\nper-tick rollup fee: ${uniqueFees.join(", ")} lamports across ${fees.length} ticks`);
check("every tick was free", uniqueFees.join(","), "0");
check("renter L1 lamports unchanged across the tick phase", run.lamports.l1SpentOnTicks, 0);

console.log(failures === 0 ? "\nall checks passed" : `\n${failures} CHECK(S) FAILED`);
process.exit(failures === 0 ? 0 : 1);
