// DCAP-verify a MagicBlock Private ER's TDX enclave. Pulls a quote against a
// fresh challenge, verifies it with the Phala QVL against the Intel PCCS, and
// enforces report_data == challenge so a stale or replayed quote fails.
import crypto from "node:crypto";
import fs from "node:fs";
import { createRequire } from "node:module";

const require = createRequire(import.meta.url);
const PCCS = process.env.PCCS_URL || "https://pccs.phala.network/tdx/certification/v4";

async function fetchJson(url, init = {}, ms = 15_000) {
  const r = await fetch(url, { ...init, signal: AbortSignal.timeout(ms) });
  const text = await r.text();
  if (!r.ok) throw new Error(`${url.split("?")[0]} -> ${r.status}: ${text.slice(0, 160)}`);
  return JSON.parse(text);
}

const joinUrl = (base, path) => new URL(path, base).toString();

/** Whether this endpoint serves TDX quotes at all (open, non-TEE ERs do not). */
export async function hasEnclave(rpcUrl) {
  try {
    const challenge = crypto.randomBytes(64).toString("base64");
    const q = await fetchJson(joinUrl(rpcUrl, `quote?challenge=${encodeURIComponent(challenge)}`), {}, 8000);
    return Boolean(q.quote);
  } catch {
    return false;
  }
}

/** Verify the enclave behind rpcUrl. Returns { validator, status, advisoryIds,
 *  mrTd, verifiedAt } or throws on any verification failure. */
export async function verifyEnclave(rpcUrl) {
  const challenge = crypto.randomBytes(64);
  const qr = await fetchJson(joinUrl(rpcUrl, `quote?challenge=${encodeURIComponent(challenge.toString("base64"))}`));
  if (!qr.quote) throw new Error("no quote in /quote response");
  const rawQuote = Uint8Array.from(Buffer.from(qr.quote, "base64"));

  const idr = await fetchJson(rpcUrl, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ jsonrpc: "2.0", id: 1, method: "getIdentity" }),
  });
  const validator = idr.result?.identity;
  if (!validator) throw new Error("getIdentity returned no validator identity");

  // The -web build's default wasm fetch fails headless; init from local bytes.
  const { default: init, js_get_collateral, js_verify } = await import("@phala/dcap-qvl-web");
  await init({ module_or_path: fs.readFileSync(require.resolve("@phala/dcap-qvl-web/dcap-qvl-web_bg.wasm")) });
  const collateral = await js_get_collateral(PCCS, rawQuote);
  const result = js_verify(rawQuote, collateral, BigInt(Math.floor(Date.now() / 1000)));
  const report = result.report?.TD10 || result.report?.TD15;
  if (!report) throw new Error("DCAP result carries no TD report");

  const reportData = String(report.report_data).replace(/^0x/, "").toLowerCase();
  if (reportData !== challenge.toString("hex")) throw new Error("report_data != challenge (stale or replayed quote)");

  return {
    validator,
    status: result.status,
    advisoryIds: result.advisory_ids ?? [],
    mrTd: Buffer.from(String(report.mr_td).replace(/^0x/, ""), "hex"),
    verifiedAt: Math.floor(Date.now() / 1000),
  };
}
