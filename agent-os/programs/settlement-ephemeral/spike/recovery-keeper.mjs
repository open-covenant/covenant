// Covenant recovery keeper — keeps delegated credit accounts recoverable automatically.
// Watches each owner's credit PDA on the pinned ER. When a delegated session goes idle,
// exceeds its max lifetime, or the validator stalls, it issues undelegate_credits to bring
// the account (with its committed balance) home to L1. No manual step: "comes home on its own."
//
// Scope: today this issues the validator-cooperative commit_and_undelegate, which recovers
// whenever the validator is reachable (idle / lifetime / soft stall). The validator-DOWN case is
// ALSO recoverable without MagicBlock: per the dev, issue an undelegation request on L1 and after
// the 30-min timeout the state unlocks permissionlessly at the last committed balance. Wiring that
// permissionless L1 request here (or adopting the simplified SDK call once it ships) is a TODO,
// not a MagicBlock dependency.
//
//   KEYPAIR=~/.config/solana/id.json node recovery-keeper.mjs
//   KEYPAIRS=a.json,b.json ER=https://eu.magicblock.app IDLE_MS=600000 node recovery-keeper.mjs
//   DRY_RUN=1 node recovery-keeper.mjs         # decide + log, send nothing
//   ONCE=1 DRY_RUN=1 node recovery-keeper.mjs  # single pass then exit (smoke test)
import fs from "node:fs";
import os from "node:os";
import crypto from "node:crypto";
import { Connection, Keypair, PublicKey, Transaction, TransactionInstruction, sendAndConfirmTransaction } from "@solana/web3.js";
import { DELEGATION_PROGRAM_ID, MAGIC_PROGRAM_ID, MAGIC_CONTEXT_ID } from "@magicblock-labs/ephemeral-rollups-sdk";
import { getAuthToken } from "@magicblock-labs/ephemeral-rollups-sdk/privacy";

const PROG = new PublicKey("cov9UDypG7nsryxdgMcKhKU2spRVWLVjxT2iTv6do5Y");
const ER_URL = process.env.ER || "https://mainnet.magicblock.app";
const POLL_MS = +(process.env.POLL_MS || 30_000);
const IDLE_MS = +(process.env.IDLE_MS || 15 * 60_000);
const MAX_LIFETIME_MS = +(process.env.MAX_LIFETIME_MS || 30 * 60_000);
const STALL_PROBES = +(process.env.STALL_PROBES || 3);
const RETRY_MS = +(process.env.RETRY_MS || 60_000);
const DRY_RUN = !!process.env.DRY_RUN;
const ONCE = !!process.env.ONCE;

const expand = (p) => p.replace(/^~/, os.homedir());
const kpPaths = (process.env.KEYPAIRS || process.env.KEYPAIR || `${os.homedir()}/.config/solana/id.json`)
  .split(",").map((s) => expand(s.trim())).filter(Boolean);
const owners = kpPaths.map((p) => Keypair.fromSecretKey(new Uint8Array(JSON.parse(fs.readFileSync(p, "utf8")))));

const l1Url = (() => { try { return fs.readFileSync("/tmp/cov-deploy-rpc", "utf8").trim(); } catch { return process.env.L1 || "https://solana-rpc.publicnode.com"; } })();
const l1 = new Connection(l1Url, "confirmed");
const signWith = (owner) => async (msg) => {
  const seed = Buffer.from(owner.secretKey.slice(0, 32));
  const der = Buffer.concat([Buffer.from("302e020100300506032b657004220420", "hex"), seed]);
  const key = crypto.createPrivateKey({ key: der, format: "der", type: "pkcs8" });
  return new Uint8Array(crypto.sign(null, Buffer.from(msg), key));
};
const erUrl = await (async () => {                   // TEE endpoints need a self-serve auth token (per-pubkey JWT)
  if (process.env.ER_TOKEN) return ER_URL + (ER_URL.includes("?") ? "&" : "?") + "token=" + process.env.ER_TOKEN;
  if (!/tee\./.test(ER_URL)) return ER_URL;
  const token = await getAuthToken(ER_URL, owners[0].publicKey, signWith(owners[0]));
  return ER_URL + (ER_URL.includes("?") ? "&" : "?") + "token=" + token;
})();
const er = new Connection(erUrl, "confirmed");

const disc = (n) => crypto.createHash("sha256").update(`global:${n}`).digest().subarray(0, 8);
const m = (pubkey, s, w) => ({ pubkey, isSigner: s, isWritable: w });
const creditsPda = (owner) => PublicKey.findProgramAddressSync([Buffer.from("credits"), owner.toBuffer()], PROG)[0];
const bal = (ai) => (ai ? ai.data.readBigUInt64LE(40) : null);
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const short = (s) => `${s.slice(0, 8)}…`;
const log = (...a) => console.log(new Date().toISOString(), ...a);
const withTimeout = (p, ms) => Promise.race([p, new Promise((_, rej) => setTimeout(() => rej(new Error("timeout")), ms))]);

const state = new Map();

async function validatorOk() {
  try { await withTimeout(er.getVersion(), 8_000); return true; } catch { return false; }
}

async function undelegate(owner, credits) {
  const ix = new TransactionInstruction({
    programId: PROG,
    keys: [m(owner.publicKey, true, true), m(credits, false, true), m(MAGIC_PROGRAM_ID, false, false), m(MAGIC_CONTEXT_ID, false, true)],
    data: disc("undelegate_credits"),
  });
  return withTimeout(sendAndConfirmTransaction(er, new Transaction().add(ix), [owner], { commitment: "confirmed" }), 30_000);
}

async function checkAccount(owner, healthFails) {
  const credits = creditsPda(owner.publicKey);
  const key = credits.toBase58();
  let st = state.get(key);
  if (!st) { st = { since: 0, bal: null, active: 0, attempt: 0 }; state.set(key, st); }

  const ai = await l1.getAccountInfo(credits).catch(() => null);
  if (!ai) return;                                    // never opened
  if (!ai.owner.equals(DELEGATION_PROGRAM_ID)) {      // home / not delegated
    if (st.since) log(`${short(key)} home (owner ${short(ai.owner.toBase58())}, bal ${bal(ai)})`);
    st.since = 0; st.bal = null; st.active = 0;
    return;
  }

  const t = Date.now();
  if (!st.since) { st.since = t; st.active = t; log(`${short(key)} delegated — watching`); }
  const erbal = bal(await er.getAccountInfo(credits).catch(() => null));
  if (erbal !== null && erbal !== st.bal) { st.bal = erbal; st.active = t; }

  const idleFor = t - st.active, lifeFor = t - st.since;
  let reason = null;
  if (lifeFor >= MAX_LIFETIME_MS) reason = `max-lifetime ${(lifeFor / 1e3) | 0}s`;
  else if (idleFor >= IDLE_MS) reason = `idle ${(idleFor / 1e3) | 0}s`;
  else if (healthFails >= STALL_PROBES) reason = `validator stall (${healthFails} probes)`;
  if (!reason) return;
  if (t - st.attempt < RETRY_MS) return;              // throttle retries
  st.attempt = t;

  log(`${short(key)} RECOVER (${reason}) erbal=${erbal}`);
  if (DRY_RUN) { log(`  DRY_RUN — would send undelegate_credits`); return; }
  try {
    const sig = await undelegate(owner, credits);
    log(`  undelegated ${sig}`);
    await sleep(8_000);
    const after = await l1.getAccountInfo(credits).catch(() => null);
    if (after && after.owner.equals(PROG)) log(`  HOME ✓ bal ${bal(after)}`);
    else log(`  still delegated — will retry`);
  } catch (e) {
    log(`  undelegate failed (${e.message}); validator may be down. fallback: permissionless L1 undelegation request + 30-min timeout (TODO)`);
  }
}

async function tick() {
  const ok = await validatorOk();
  tick.fails = ok ? 0 : (tick.fails || 0) + 1;
  if (!ok) log(`validator probe failed (${tick.fails}/${STALL_PROBES}) ${ER_URL}`);
  for (const o of owners) {
    try { await checkAccount(o, tick.fails); } catch (e) { log(`check error ${short(o.publicKey.toBase58())}: ${e.message}`); }
  }
}

log(`recovery-keeper up — ER ${ER_URL} — ${owners.length} account(s) — idle ${IDLE_MS}ms lifetime ${MAX_LIFETIME_MS}ms poll ${POLL_MS}ms${DRY_RUN ? " [DRY_RUN]" : ""}`);
for (const o of owners) log(`  watch ${creditsPda(o.publicKey).toBase58()}  owner ${o.publicKey.toBase58()}`);
log(`  L1 ${l1Url.replace(/\?.*$/, "?…")}`);
do { await tick(); if (!ONCE) await sleep(POLL_MS); } while (!ONCE);
