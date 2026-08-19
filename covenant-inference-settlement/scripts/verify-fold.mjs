// Recompute the provenance hash-chain off-chain from the gateway's receipts and
// assert it equals the on-chain root the MagicBlock ER folded. This is the
// reference-run proof: the committed root IS the hash-chain of the served receipts.
//
//   GATEWAY_URL, BRIDGE_URL, ROOT_BEFORE(hex), EXPECT_N -> exit 0 on match.

import { foldReceipts } from "../settlement.mjs";

const env = (k, d) => process.env[k] ?? d;
const GATEWAY_URL = env("GATEWAY_URL", "http://127.0.0.1:28091");
const BRIDGE_URL = env("BRIDGE_URL", "http://127.0.0.1:8799");
const ROOT_BEFORE = env("ROOT_BEFORE", "00".repeat(32));
const EXPECT_N = Number(env("EXPECT_N", "30"));

const getJson = async (url) => {
  const r = await fetch(url);
  if (!r.ok) throw new Error(`${url} -> ${r.status}`);
  return r.json();
};

const page = await getJson(`${GATEWAY_URL}/v1/receipts?limit=1000`);
const folded = page.data
  .filter((r) => r.settlement && typeof r.settlement.position === "number")
  .sort((a, b) => a.settlement.position - b.settlement.position);

console.log(`receipts: ${page.data.length} total, ${folded.length} folded into the provenance root`);
if (folded.length < EXPECT_N) {
  console.log(`WAITING: only ${folded.length}/${EXPECT_N} folded so far`);
  process.exit(2);
}

const hashes = folded.map((r) => Buffer.from(r.receipt_hash, "hex"));
const expected = foldReceipts(Buffer.from(ROOT_BEFORE, "hex"), hashes).toString("hex");

const bridgeRoot = await getJson(`${BRIDGE_URL}/root`);
const onchain = bridgeRoot.er_root_hex ?? bridgeRoot.l1_root_hex;

const sampleSigs = folded.slice(0, 3).map((r) => `#${r.settlement.position} ${r.settlement.er_sig}`);
console.log(`root before      ${ROOT_BEFORE}`);
console.log(`sample ER sigs   ${sampleSigs.join("\n                 ")}`);
console.log(`positions        ${folded[0].settlement.position}..${folded.at(-1).settlement.position}`);
console.log(`off-chain fold   ${expected}`);
console.log(`on-chain root    ${onchain}`);
const match = expected === onchain;
console.log(`provenance check ${match ? "MATCH — on-chain root IS the hash-chain of the served receipts" : "MISMATCH"}`);
process.exit(match ? 0 : 1);
