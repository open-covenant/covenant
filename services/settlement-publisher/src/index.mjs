// Covenant settlement publisher (sidecar)
//
// Watches the sandbox daemon's audit feed for new `intent_dispatched`
// events and posts a `consume_credits(1)` transaction on devnet against
// the deployed settlement program. The resulting tx signature is stored
// keyed by intent_id and served over a tiny HTTP API so the web UI can
// surface a Solscan link beside each task.
//
// Why a sidecar and not in covenantd: the daemon today carries no
// `solana-client` dep and no chain publisher. This service is the
// pragmatic path to demonstrate real on-chain settlement without a
// dep-tree-heavy refactor in the daemon. Long-term home is inside
// `covenant-settlement` once the publisher lands there.

// @coral-xyz/anchor ships as CommonJS; Node 20 ESM resolution can only
// see its default export. Destructure off the default to get the real
// classes/values.
import anchorPkg from "@coral-xyz/anchor";
const { AnchorProvider, Program, Wallet, BN } = anchorPkg;
import {
  Connection,
  Keypair,
  PublicKey,
  SystemProgram,
} from "@solana/web3.js";
import http from "node:http";
import { readFileSync, mkdirSync, appendFileSync, existsSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { parseOperatorKeypairBytes } from "./keypair.mjs";
import { restoreSigs } from "./sigs-replay.mjs";
import { selectDispatches } from "./dispatch-select.mjs";

const HERE = dirname(fileURLToPath(import.meta.url));

// ─── config from env ─────────────────────────────────────────────────

const DAEMON_URL =
  process.env.COVENANT_DAEMON_URL ?? "http://covenant-sandbox-daemon:8421";
const OPERATOR_TOKEN = process.env.COVENANT_OPERATOR_TOKEN ?? "";
const RPC_URL =
  process.env.COVENANT_SOLANA_RPC_URL ?? "https://api.devnet.solana.com";
const CLUSTER = process.env.COVENANT_SOLANA_CLUSTER ?? "devnet";

// On-chain addresses are baked in for the current devnet deployment.
// Each is overridable via env so this same service can flip to mainnet
// when that day comes without a code change.
const PROGRAM_ID = new PublicKey(
  process.env.COVENANT_SETTLEMENT_PROGRAM_ID ??
    "cov9UDypG7nsryxdgMcKhKU2spRVWLVjxT2iTv6do5Y",
);
const CONFIG = new PublicKey(
  process.env.COVENANT_SETTLEMENT_CONFIG ??
    "BGGx99dV5LU2GpKCmhXqT1mi1yNr8EuMuMd5BAG7Lcvi",
);
const CREDIT_ACCOUNT = new PublicKey(
  process.env.COVENANT_SETTLEMENT_CREDIT_ACCOUNT ??
    "AM3LpxSGVjCnZqHJN9YEZs2VQUd5dnrf5QJtRav53uB8",
);

// Operator keypair: raw `[u8; 64]` JSON, same shape solana-keygen writes.
// Loaded from env so the wallet never lives on a disk image.
function loadOperatorKeypair() {
  return Keypair.fromSecretKey(parseOperatorKeypairBytes());
}

const POLL_MS = Number(process.env.SETTLE_POLL_MS ?? "4000");
const SETTLE_AMOUNT = new BN(process.env.SETTLE_AMOUNT_CREDITS ?? "1");
const STATE_DIR = process.env.SETTLE_STATE_DIR ?? "/data/settlement-publisher";
const HTTP_PORT = Number(process.env.PORT ?? "10001");

// ─── state ────────────────────────────────────────────────────────────
//
// In-memory + jsonl-on-disk. Disk is Render's persistent mount so a
// restart doesn't drop the sig table. The jsonl is append-only,
// idempotent on replay.

mkdirSync(STATE_DIR, { recursive: true });
const SIGS_PATH = resolve(STATE_DIR, "sigs.jsonl");
const sigByIntent = new Map(); // intent_id -> { tx_sig, slot, settled_at_ms, intent_text }
const processedIntents = new Set();

if (existsSync(SIGS_PATH)) {
  const restored = restoreSigs(readFileSync(SIGS_PATH, "utf8"));
  for (const [intentId, row] of restored) {
    sigByIntent.set(intentId, row);
    processedIntents.add(intentId);
  }
  console.log(`[init] restored ${sigByIntent.size} sigs from disk`);
}

function recordSig(row) {
  sigByIntent.set(row.intent_id, row);
  processedIntents.add(row.intent_id);
  appendFileSync(SIGS_PATH, JSON.stringify(row) + "\n");
}

// ─── anchor wiring ────────────────────────────────────────────────────

const operator = loadOperatorKeypair();
const wallet = new Wallet(operator);
const conn = new Connection(RPC_URL, "confirmed");
const provider = new AnchorProvider(conn, wallet, { commitment: "confirmed" });

const idl = JSON.parse(
  readFileSync(resolve(HERE, "..", "settlement.idl.json"), "utf8"),
);
idl.address = PROGRAM_ID.toBase58();
const program = new Program(idl, provider);

console.log("[boot] operator wallet:", operator.publicKey.toBase58());
console.log("[boot] program:", PROGRAM_ID.toBase58());
console.log("[boot] config:", CONFIG.toBase58());
console.log("[boot] credit account:", CREDIT_ACCOUNT.toBase58());
console.log("[boot] daemon:", DAEMON_URL);
console.log("[boot] rpc:", RPC_URL);

// ─── publish loop ─────────────────────────────────────────────────────

async function consumeOne(intentId, intentText) {
  // The IDL declares consume_credits(amount: u64, receipt_hash: [u8;32]).
  // The receipt_hash is a 32-byte payload the program binds the debit
  // to; we use the audit intent_id sha256'd into 32 bytes so each
  // on-chain settlement is provably tied to the corresponding audit row
  // (and replays of the same intent dedup at the program level by hash).
  const { createHash } = await import("node:crypto");
  const receiptHash = Array.from(
    createHash("sha256").update(intentId).digest(),
  );
  const sig = await program.methods
    .consumeCredits(SETTLE_AMOUNT, receiptHash)
    .accounts({
      config: CONFIG,
      credits: CREDIT_ACCOUNT,
      owner: operator.publicKey,
    })
    .rpc();
  // Best-effort confirmation slot — solana's signature subscribe path
  // would be more accurate but a single getSignatureStatuses call is
  // fine for the sidecar's surfaceable info.
  let slot = null;
  try {
    const st = await conn.getSignatureStatuses([sig]);
    slot = st.value[0]?.slot ?? null;
  } catch {
    // best-effort; the sig is what matters
  }
  recordSig({
    intent_id: intentId,
    tx_sig: sig,
    slot,
    settled_at_ms: Date.now(),
    intent_text: intentText?.slice(0, 200) ?? null,
    cluster: CLUSTER,
  });
  return sig;
}

async function pollOnce() {
  // Plain concat -- DAEMON_URL may itself contain a path prefix
  // (e.g. .../api/covenant on the public proxy) which `new URL` would
  // strip when given an absolute-path relative URL.
  const base = DAEMON_URL.replace(/\/+$/, "");
  const url = `${base}/audit/recent?limit=100`;
  const res = await fetch(url, {
    headers: OPERATOR_TOKEN
      ? { Authorization: `Bearer ${OPERATOR_TOKEN}` }
      : {},
  });
  if (!res.ok) {
    throw new Error(`daemon audit fetch ${res.status}: ${await res.text()}`);
  }
  const body = await res.json();
  const events = body?.events ?? [];

  // Walk oldest -> newest so the order on-chain mirrors the order
  // tasks actually fired in the sandbox.
  const dispatches = selectDispatches(events);

  let published = 0;
  for (const e of dispatches) {
    const intentId = e.kind?.intent_id;
    if (!intentId || processedIntents.has(intentId)) continue;
    const intentText = e.kind?.intent_text ?? null;
    try {
      const sig = await consumeOne(intentId, intentText);
      console.log(
        `[publish] intent=${intentId.slice(0, 8)} sig=${sig.slice(0, 12)}...`,
      );
      published++;
    } catch (err) {
      // Mark as processed even on failure so a poisoned intent doesn't
      // block the queue forever. The daemon's audit row still proves
      // the task ran; only the on-chain anchor is missing.
      processedIntents.add(intentId);
      const msg = err instanceof Error ? err.message : String(err);
      console.error(
        `[publish] intent=${intentId.slice(0, 8)} FAILED: ${msg.slice(0, 200)}`,
      );
    }
  }
  return published;
}

async function loop() {
  for (;;) {
    try {
      await pollOnce();
    } catch (err) {
      const msg = err instanceof Error ? err.message : String(err);
      console.error(`[poll] error: ${msg.slice(0, 200)}`);
    }
    await new Promise((r) => setTimeout(r, POLL_MS));
  }
}

// ─── tiny HTTP server ─────────────────────────────────────────────────

const server = http.createServer((req, res) => {
  const url = new URL(req.url ?? "/", "http://x");
  if (url.pathname === "/healthz") {
    res.writeHead(200, { "Content-Type": "application/json" });
    res.end(
      JSON.stringify({
        ok: true,
        operator: operator.publicKey.toBase58(),
        cluster: CLUSTER,
        published: sigByIntent.size,
      }),
    );
    return;
  }
  if (url.pathname === "/sigs") {
    // Return everything as a map keyed by intent_id so the web app can
    // O(1) look up a sig per task it's rendering.
    const out = {};
    for (const [k, v] of sigByIntent) out[k] = v;
    res.writeHead(200, {
      "Content-Type": "application/json",
      "Cache-Control": "no-store",
      "Access-Control-Allow-Origin": "*",
    });
    res.end(JSON.stringify({ cluster: CLUSTER, sigs: out }));
    return;
  }
  if (url.pathname.startsWith("/sigs/")) {
    const intentId = decodeURIComponent(url.pathname.slice("/sigs/".length));
    const row = sigByIntent.get(intentId) ?? null;
    res.writeHead(200, {
      "Content-Type": "application/json",
      "Cache-Control": "no-store",
      "Access-Control-Allow-Origin": "*",
    });
    res.end(JSON.stringify(row));
    return;
  }
  res.writeHead(404);
  res.end();
});
server.listen(HTTP_PORT, "0.0.0.0", () => {
  console.log(`[http] listening on :${HTTP_PORT}`);
});

loop();
