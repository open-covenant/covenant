#!/usr/bin/env node
// Bento guard sidecar for the Covenant daemon.
//
// Reads a screen request as JSON on stdin, runs Bento's protect() through the
// real @bentoguard/sdk, and writes a normalized AnalysisResult as JSON on
// stdout. The agent key lives only here: the daemon forwards a keypair FILE
// PATH (AGENT_WALLET_KEYPAIR_PATH), never the key value. Fails closed: any
// error exits non-zero so the caller blocks, with the key redacted from stderr.

import { readFileSync } from "node:fs";
import bs58 from "bs58";
import { BentoClient, protect } from "@bentoguard/sdk";

// stdout carries only the JSON verdict the caller parses. The SDK writes
// progress lines via console.log (stdout), which would corrupt that, so route
// console output to stderr and leave stdout for the single explicit write.
console.log = (...a) => process.stderr.write(a.join(" ") + "\n");
console.info = console.log;

// Hoisted so the error handler can redact it from any message.
let loadedKey = null;

function readStdin() {
  return new Promise((resolve, reject) => {
    let data = "";
    process.stdin.setEncoding("utf8");
    process.stdin.on("data", (c) => (data += c));
    process.stdin.on("end", () => resolve(data));
    process.stdin.on("error", reject);
  });
}

// A base58 private-key string, loaded from the keypair file the daemon points
// us at. The file holds either the base58 string or a Solana keypair JSON array.
function loadAgentKey() {
  const path = process.env.AGENT_WALLET_KEYPAIR_PATH;
  if (!path) throw new Error("AGENT_WALLET_KEYPAIR_PATH is not set");
  const raw = readFileSync(path, "utf8").trim();
  loadedKey = raw.startsWith("[") ? bs58.encode(Uint8Array.from(JSON.parse(raw))) : raw;
  return loadedKey;
}

// Map the SDK's recommendation onto the exact contract the Rust side decodes,
// tolerant of verb vs participle. Throw on anything unrecognized so a contract
// drift surfaces as an explicit failure (which fails closed) rather than a
// silent mis-decode.
function normalizeRecommendation(raw) {
  switch (String(raw ?? "").trim().toUpperCase()) {
    case "ALLOW":
    case "ALLOWED":
      return "ALLOW";
    case "BLOCK":
    case "BLOCKED":
      return "BLOCKED";
    case "ESCALATE":
    case "ESCALATED":
      return "ESCALATED";
    default:
      throw new Error(`unrecognized recommendation: ${JSON.stringify(raw)}`);
  }
}

// The SDK does not keep riskScore on one scale across versions: 1.2.8 sent
// raw_score/100000 (a 0-1 fraction), 1.2.9 forwards the relayer's final_score
// untouched. Accept the scales seen in the wild (0-1 fraction, 0-100 percent,
// raw 0-100000) and map onto the 0-100 integer contract, clamping instead of
// saturating so a scale drift can never masquerade as max risk.
function clampRisk(raw) {
  const n = Number(raw ?? 0);
  if (!Number.isFinite(n) || n < 0) return 0;
  const scaled = n <= 1 ? n * 100 : n <= 100 ? n : n / 1000;
  return Math.min(100, Math.max(0, Math.round(scaled)));
}

function buildOutput(verdict) {
  const out = {
    recommendation: normalizeRecommendation(verdict.recommendation),
    riskScore: clampRisk(verdict.riskScore),
    reasoning: String(verdict.reasoning ?? ""),
  };
  for (const k of ["actionId", "approveUrl", "blockUrl", "reviewUrl", "timestamp"]) {
    if (verdict[k] != null) out[k] = String(verdict[k]);
  }
  return out;
}

async function main() {
  const req = JSON.parse(await readStdin());
  if (typeof req.intent !== "string" || req.intent.length === 0) {
    throw new Error('missing string "intent"');
  }

  const key = loadAgentKey();
  const timeout = req.timeoutMs ?? 8000;
  BentoClient.initialize({ agentWalletPrivateKey: key, timeout });

  try {
    const verdict = await protect(req.intent, {
      agentAddress: req.agentAddress,
      timeout,
      // Since 1.2.9 the verdict arrives over an SSE stream that otherwise
      // waits POLL_TIMEOUT_MS (5 min) for a decision. Bound it to the same
      // budget as the rest of the call so we return (or fail closed) before
      // the daemon's hard-kill margin, never hanging on a silent stream.
      pollTimeoutMs: timeout,
      autoPollEscalation: false,
      silent: true,
    });
    process.stdout.write(JSON.stringify(buildOutput(verdict)));
  } catch (e) {
    // SDK <=1.2.8 threw on a BLOCKED verdict (HIGH_RISK_DETECTED) with the
    // verdict in e.details; 1.2.9 returns BLOCKED through the normal path
    // above. Keep the mapping so an older lockfile or a relayer that still
    // emits the legacy error decodes as a real block, not a guard failure.
    if (e?.code === "HIGH_RISK_DETECTED") {
      const d = e.details && typeof e.details === "object" ? e.details : {};
      process.stdout.write(
        JSON.stringify(
          buildOutput({
            recommendation: "BLOCKED",
            riskScore: d.riskScore,
            reasoning: d.reasoning ?? e.message ?? "blocked",
            actionId: d.actionId,
            approveUrl: d.approveUrl,
            blockUrl: d.blockUrl,
            reviewUrl: d.reviewUrl,
            timestamp: d.timestamp,
          }),
        ),
      );
      return;
    }
    throw e;
  }
}

main().catch((e) => {
  let msg = e instanceof Error ? e.message : String(e);
  if (loadedKey && loadedKey.length > 6) msg = msg.split(loadedKey).join("[redacted]");
  process.stderr.write(`bento guard: ${msg}`);
  process.exitCode = 1;
});
