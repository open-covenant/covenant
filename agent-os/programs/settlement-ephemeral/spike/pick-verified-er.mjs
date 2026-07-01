// Agent-side discovery: ask the Magic Router for its ERs, then keep only the
// ones a Covenant credential has attested as verified. This is the whole point
// of the registry: an agent picks an execution endpoint and can check the trust
// signal in one account read per validator, no router change, no indexer.
//
//   node pick-verified-er.mjs                 # mainnet router, devnet attestations (the demo split)
//   ROUTER=... SAS_RPC=... COVENANT_ISSUER=... node pick-verified-er.mjs
import { rpcFor, resolveEr } from "./er-registry.mjs";

const ROUTER = process.env.ROUTER || "https://router.magicblock.app";
const SAS_RPC = process.env.SAS_RPC || "https://api.devnet.solana.com";
const ISSUER = process.env.COVENANT_ISSUER || "AdChcSmDKX57rU9qChMJ3MKnqNZbmiQAjuns9VCjzqRb";

const routes = (await (await fetch(ROUTER, {
  method: "POST", headers: { "content-type": "application/json" },
  body: JSON.stringify({ jsonrpc: "2.0", id: 1, method: "getRoutes" }),
})).json()).result;

const rpc = rpcFor(SAS_RPC);
console.log(`router ${ROUTER}: ${routes.length} ERs`);
console.log(`Covenant issuer ${ISSUER} via ${SAS_RPC}\n`);

const checked = [];
for (const v of routes) {
  const r = await resolveEr(rpc, ISSUER, v.identity);
  checked.push({ ...v, verified: r.verified, tcb: r.data?.status });
  console.log(`  ${r.verified ? "OK  verified " : "    unverified"}  ${v.identity}  ${v.fqdn}  ${v.countryCode}${r.verified ? `  [TCB ${r.data.status}]` : ""}`);
}

const verified = checked.filter((v) => v.verified);
console.log(`\n${verified.length}/${checked.length} Covenant-verified.`);
if (verified.length) console.log(`agent picks -> ${verified[0].fqdn} (${verified[0].identity})`);
