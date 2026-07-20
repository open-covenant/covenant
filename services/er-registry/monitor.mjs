// Covenant verified-ER registry monitor. One pass:
//
//   1. ask the Magic Router which ERs exist
//   2. for each ER that serves TDX quotes, DCAP-verify its enclave
//   3. keep its SAS attestation fresh: replace it when the measurement changed
//      or it is inside the renewal window
//
// Attestations expire (TTL_HOURS). A verification failure never force-closes an
// attestation (transient network trouble should not kill a valid one); the TTL
// bounds how long a no-longer-verifiable enclave can stay marked verified. If
// this monitor stops running entirely, every attestation lapses and resolvers
// fail closed to unverified. That makes the registry honest by construction.
//
// Runs as a scheduled job (Render cron). Env:
//   ER_MONITOR_KEY / ER_MONITOR_KEY_FILE  signer (authorized signer of the credential)
//   RPC_URL                               Solana RPC (mainnet)
//   ROUTER                                Magic Router (default mainnet)
//   COVENANT_ISSUER                       credential authority (default AdChc…)
//   PCCS_URL                              Intel PCCS for DCAP collateral
//   TTL_HOURS / RENEW_BEFORE_HOURS        attestation lifetime / renewal window (72 / 48)
import { hasEnclave, verifyEnclave } from "./tee.mjs";
import { rpcFor, signerFromEnv, currentAttestation, refreshAttestation } from "./sas.mjs";

const ROUTER = process.env.ROUTER || "https://router.magicblock.app";
const RPC_URL = process.env.RPC_URL || "https://api.mainnet-beta.solana.com";
const ISSUER = process.env.COVENANT_ISSUER || "AdChcSmDKX57rU9qChMJ3MKnqNZbmiQAjuns9VCjzqRb";
const TTL_S = Number(process.env.TTL_HOURS || 72) * 3600;
const RENEW_BEFORE_S = Number(process.env.RENEW_BEFORE_HOURS || 48) * 3600;
if (!Number.isFinite(TTL_S) || !Number.isFinite(RENEW_BEFORE_S)) {
  throw new Error("TTL_HOURS and RENEW_BEFORE_HOURS must be numeric");
}

const res = await fetch(ROUTER, {
  method: "POST",
  headers: { "content-type": "application/json" },
  body: JSON.stringify({ jsonrpc: "2.0", id: 1, method: "getRoutes" }),
  signal: AbortSignal.timeout(10_000),
});
if (!res.ok) throw new Error(`${ROUTER} -> ${res.status}`);
const body = await res.json();
if (body.error) throw new Error(`getRoutes error: ${JSON.stringify(body.error)}`);
const routes = body.result;
if (!Array.isArray(routes)) throw new Error("getRoutes returned no route list");
if (routes.length === 0) {
  console.error("getRoutes returned zero ERs");
  process.exit(1);
}

const rpc = rpcFor(RPC_URL);
const signer = await signerFromEnv();
const now = Math.floor(Date.now() / 1000);
console.log(`er-registry monitor | ${routes.length} ERs from ${ROUTER} | signer ${signer.address}`);

const causeOf = (e) => (e.cause ? ` (${e.cause.message ?? e.cause})` : "");
let failures = 0;
for (const r of routes) {
  if (!r || typeof r.identity !== "string" || typeof r.fqdn !== "string") {
    failures += 1;
    console.error(`  FAIL      malformed route entry: ${JSON.stringify(r)}`);
    continue;
  }
  const label = `${r.identity} ${r.fqdn}`;
  try {
    if (!(await hasEnclave(r.fqdn))) {
      console.log(`  skip      ${label} (no enclave)`);
      continue;
    }
    const cur = await currentAttestation(rpc, ISSUER, r.identity);
    const v = await verifyEnclave(r.fqdn);
    if (v.validator !== r.identity) throw new Error(`identity mismatch: enclave says ${v.validator}`);
    const endpoint = new URL(r.fqdn).host;

    const fresh = cur.exists && cur.expiry !== 0 && cur.expiry - now > RENEW_BEFORE_S;
    const unchanged = cur.exists && cur.mrTd === v.mrTd.toString("hex") && cur.status === v.status && cur.endpoint === endpoint;
    if (fresh && unchanged) {
      console.log(`  fresh     ${label} (TCB ${v.status}, expires in ${((cur.expiry - now) / 3600).toFixed(1)}h)`);
      continue;
    }
    const { sig } = await refreshAttestation(rpc, signer, r.identity, { ...v, endpoint }, TTL_S, cur);
    const advisories = v.advisoryIds.length ? `, advisories ${v.advisoryIds.join("/")}` : "";
    console.log(`  refreshed ${label} (TCB ${v.status}${advisories}, mr_td ${v.mrTd.toString("hex").slice(0, 16)}…, expiry +${TTL_S / 3600}h) ${sig}`);
  } catch (e) {
    failures += 1;
    console.error(`  FAIL      ${label}: ${e.message}${causeOf(e)}`);
  }
}

console.log(failures ? `done with ${failures} failure(s)` : "done");
process.exit(failures ? 1 : 0);
