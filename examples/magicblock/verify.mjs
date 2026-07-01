// Read-only demo of the Covenant trust layer over MagicBlock. No keys, no funds.
// Runs against Solana mainnet and does two things:
//   1. discovers which MagicBlock ERs are Covenant-verified (router + SAS)
//   2. verifies that a reference agent's on-chain provenance root is the
//      hash-chain of its real answers
//
//   npm install && node verify.mjs
//   RPC=<your-mainnet-rpc> node verify.mjs      # if the public RPC rate-limits
import { Connection, PublicKey } from "@solana/web3.js";
import crypto from "node:crypto";
import fs from "node:fs";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";

const RPC = process.env.RPC || "https://api.mainnet-beta.solana.com";
const ROUTER = process.env.ROUTER || "https://router.magicblock.app";
const PROGRAM = "cov9UDypG7nsryxdgMcKhKU2spRVWLVjxT2iTv6do5Y";
const SAS = new PublicKey("22zoJMtdu4tQc2PzL74ZUT7FrwgB1Udec8DdW4yw4BdG");
const ISSUER = new PublicKey("AdChcSmDKX57rU9qChMJ3MKnqNZbmiQAjuns9VCjzqRb");

const work = JSON.parse(fs.readFileSync(join(dirname(fileURLToPath(import.meta.url)), "work-items.json"), "utf8"));
const c = new Connection(RPC, "confirmed");
const sha256 = (...b) => crypto.createHash("sha256").update(Buffer.concat(b)).digest();
const canon = (o) => JSON.stringify(Object.keys(o).sort().reduce((a, k) => ((a[k] = o[k]), a), {}));
const sasPda = (seeds) => PublicKey.findProgramAddressSync(seeds, SAS)[0];

const credential = sasPda([Buffer.from("credential"), ISSUER.toBuffer(), Buffer.from("covenant")]);
const schema = sasPda([Buffer.from("schema"), credential.toBuffer(), Buffer.from("er-verified"), Buffer.from([1])]);

async function isVerified(validator) {
  const att = sasPda([Buffer.from("attestation"), credential.toBuffer(), schema.toBuffer(), new PublicKey(validator).toBuffer()]);
  const ai = await c.getAccountInfo(att);
  if (!ai || !ai.owner.equals(SAS)) return false;
  return ai.data[1 + 32 + 32 + 32 + 4] === 1; // disc + nonce + credential + schema + data_len, then the `verified` bool
}

console.log(`Covenant × MagicBlock, read-only mainnet demo`);
console.log(`program ${PROGRAM}\n`);

console.log(`1. Which MagicBlock ERs are Covenant-verified?`);
const routes = (await (await fetch(ROUTER, {
  method: "POST", headers: { "content-type": "application/json" },
  body: JSON.stringify({ jsonrpc: "2.0", id: 1, method: "getRoutes" }),
})).json()).result;
for (const r of routes) {
  const v = await isVerified(r.identity);
  console.log(`   ${v ? "verified  " : "unverified"}  ${r.fqdn}  ${r.identity}`);
}

console.log(`\n2. Is the reference agent's on-chain record its real work?`);
let root = Buffer.alloc(32);
for (const it of work.items) {
  root = sha256(root, sha256(Buffer.from(canon({ model: work.model, intent: it.intent, reply: it.reply }))));
}
const ai = await c.getAccountInfo(new PublicKey(work.credits));
const onChain = Buffer.from(ai.data.subarray(49, 81));
const match = root.equals(onChain);
console.log(`   recomputed from ${work.items.length} answers : ${root.toString("hex")}`);
console.log(`   on-chain provenance_root      : ${onChain.toString("hex")}`);
console.log(`   ${match ? "match, the on-chain root is the hash-chain of the agent's real answers" : "MISMATCH"}`);

process.exit(match ? 0 : 1);
