// Live mainnet demo of the Covenant verified-enclave badge for Loofta Pay.
// Resolves the MagicBlock Private ER Loofta settles on, prints the badge, and
// writes a standalone HTML preview. Read-only, no keys, no funds.
//
//   node demo.mjs
//   SOLANA_RPC=<url> VALIDATOR=<er-identity> node demo.mjs

import fs from "node:fs";
import { verifyEnclave, routeAndVerify, badgeHtml, badgeText } from "./badge.mjs";

const RPC = process.env.SOLANA_RPC || "https://api.mainnet-beta.solana.com";
// The MagicBlock Private ER (TDX enclave) Loofta settles on today.
const VALIDATOR = process.env.VALIDATOR || "MTEWGuqxUpYZGFJQcp8tLN7x5v9BSeoFHYWQQ3n3xzo";

console.log(`resolving Covenant attestation for ${VALIDATOR}\n`);
const data = await verifyEnclave({ rpcUrl: RPC, validator: VALIDATOR });
console.log(badgeText(data));
console.log(JSON.stringify(data, null, 2));

// Route-and-prove: let Covenant pick a verified ER and hand back the badge.
console.log("\nroute-and-verify (Covenant picks the enclave):");
const routed = await routeAndVerify({ rpcUrl: RPC });
console.log(`  endpoint  ${routed.endpoint}`);
console.log(`  ${badgeText(routed.badge)}`);
console.log(
  `  routes: ${routed.routes.length}, verified: ${routed.routes.filter((r) => r.covenant.verified).length}`,
);

const page = `<!doctype html>
<meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1">
<title>Covenant verified-enclave badge</title>
<body style="margin:0;padding:32px;background:#fff;font-family:-apple-system,system-ui,sans-serif">
<h3 style="margin:0 0 4px">Checkout / receipt, compact</h3>
<div style="margin-bottom:24px">${badgeHtml(data, { compact: true })}</div>
<h3 style="margin:0 0 8px">Receipt, expanded (light)</h3>
<div style="margin-bottom:24px">${badgeHtml(data, { theme: "light" })}</div>
<h3 style="margin:0 0 8px">Receipt, expanded (dark)</h3>
<div style="padding:16px;background:#0C111D;border-radius:12px;display:inline-block">${badgeHtml(data, { theme: "dark" })}</div>
</body>`;
fs.writeFileSync(new URL("./badge-preview.html", import.meta.url), page);
console.log("\nwrote badge-preview.html");
