// er-guard: session-reliability keeper for MagicBlock ephemeral rollups.
//
// Watches delegated accounts and, while the validator is reachable, sends a
// cooperative undelegate back to L1 on idle, max-lifetime, or a soft stall —
// the "should only be a fallback, never actually trigger" posture: undelegate
// while healthy so the hard-recovery path never has to fire.
//
// When the validator is unreachable the cooperative undelegate cannot land.
// The permissionless escape hatch is dlp 3.1.0's RequestUndelegation +
// timeout, but that instruction requires the DELEGATED ACCOUNT as a signer —
// for a program-owned PDA only the owner program can do that, via CPI with the
// PDA's seeds. A client cannot issue it. The guard therefore calls the
// `requestUndelegate` hook when the owner program exposes such an instruction,
// and otherwise reports exactly what is stuck. State is never at risk either
// way: the last committed state is final on L1.
import { Connection, PublicKey } from "@solana/web3.js";
import { DELEGATION_PROGRAM_ID, getAuthToken } from "@magicblock-labs/ephemeral-rollups-sdk";
import crypto from "node:crypto";

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const withTimeout = (p, ms) => {
  let t;
  const timeout = new Promise((_, rej) => { t = setTimeout(() => rej(new Error("timeout")), ms); });
  return Promise.race([p, timeout]).finally(() => clearTimeout(t));
};
const short = (s) => `${String(s).slice(0, 8)}…`;

const edSignerFor = (keypair) => async (msg) => {
  const der = Buffer.concat([Buffer.from("302e020100300506032b657004220420", "hex"), Buffer.from(keypair.secretKey.slice(0, 32))]);
  return new Uint8Array(crypto.sign(null, Buffer.from(msg), crypto.createPrivateKey({ key: der, format: "der", type: "pkcs8" })));
};

/**
 * Watch delegated accounts on one ER.
 *
 * opts:
 *   l1                  Connection to Solana L1
 *   erUrl               ER endpoint (a *tee* host auto-mints and refreshes a JWT)
 *   accounts            [{ address, label?, undelegate?, requestUndelegate?, isActive? }]
 *                         undelegate(er)        cooperative path, owner-signed, program-specific
 *                         requestUndelegate(l1) permissionless path via the owner program's CPI ix
 *                         isActive(erAccountInfo, prev) -> value that changes while the session is live
 *   tokenSigner         { publicKey, sign } for TEE token minting (defaults to none)
 *   policy              { idleMs, maxLifetimeMs, stallProbes, pollMs, retryMs }
 *   dryRun, once, log
 */
export async function guard(opts) {
  const {
    l1, erUrl, accounts, tokenSigner = null,
    policy = {}, dryRun = false, once = false,
    log = (...a) => console.log(new Date().toISOString(), ...a),
  } = opts;
  const { idleMs = 15 * 60_000, maxLifetimeMs = 30 * 60_000, stallProbes = 3, pollMs = 30_000, retryMs = 60_000 } = policy;
  const isTee = /tee\./.test(erUrl);

  if (!Array.isArray(accounts) || accounts.length === 0) throw new Error("guard requires a non-empty accounts array");
  if (isTee && !tokenSigner) throw new Error("a *tee* endpoint needs a tokenSigner to mint its JWT");
  for (const a of accounts) a.address = new PublicKey(a.address);

  let er = null;
  let tokenExp = 0;
  async function ensureEr() {
    if (er && (!isTee || tokenExp === 0 || Date.now() / 1000 < tokenExp - 60)) return;
    let url = erUrl;
    if (isTee) {
      const token = await withTimeout(getAuthToken(erUrl, tokenSigner.publicKey, tokenSigner.sign), 10_000);
      try { tokenExp = JSON.parse(Buffer.from(token.split(".")[1], "base64url").toString()).exp || 0; } catch { tokenExp = 0; }
      url += (url.includes("?") ? "&" : "?") + "token=" + token;
      log(`minted TEE token (exp ${tokenExp || "n/a"})`);
    }
    er = new Connection(url, "confirmed");
  }

  const state = new Map();
  let stallFails = 0;

  async function check(acct) {
    const key = String(acct.address);
    let st = state.get(key);
    if (!st) { st = { since: 0, activity: null, active: 0, attempt: 0, seen: false }; state.set(key, st); }

    const ai = await l1.getAccountInfo(acct.address);
    if (!ai) { if (!st.seen) { st.seen = true; log(`${short(key)} not opened`); } return; }
    if (!ai.owner.equals(DELEGATION_PROGRAM_ID)) {
      // Log the transition home, and once on first sight so a run never looks silent.
      if (st.since || !st.seen) log(`${short(key)} home (owner ${short(ai.owner.toBase58())})`);
      st.since = 0; st.activity = null; st.seen = true;
      return;
    }

    const t = Date.now();
    if (!st.since) { st.since = t; st.active = t; log(`${short(key)} delegated, watching`); }
    const erAi = er ? await er.getAccountInfo(acct.address).catch(() => null) : null;
    const activity = acct.isActive ? acct.isActive(erAi, st.activity) : erAi?.data?.length ? crypto.createHash("sha256").update(erAi.data).digest("hex") : null;
    if (activity !== null && activity !== st.activity) { st.activity = activity; st.active = t; }

    // A stalled validator takes priority: a cooperative undelegate routes
    // through the ER and cannot land while it is unreachable, so idle and
    // max-lifetime must not send it into a dead endpoint.
    const idleFor = t - st.active, lifeFor = t - st.since;
    const stalled = stallFails >= stallProbes;
    let reason = null;
    if (stalled) reason = `validator stall (${stallFails} probes)`;
    else if (lifeFor >= maxLifetimeMs) reason = `max-lifetime ${(lifeFor / 1e3) | 0}s`;
    else if (idleFor >= idleMs) reason = `idle ${(idleFor / 1e3) | 0}s`;
    if (!reason || t - st.attempt < retryMs) return;
    st.attempt = t;

    log(`${short(key)} RECOVER (${reason})`);
    if (stalled) {
      if (acct.requestUndelegate) {
        if (dryRun) { log(`  DRY_RUN, would send the owner program's permissionless undelegation request`); return; }
        try { log(`  requested permissionless undelegation ${await acct.requestUndelegate(l1)}`); }
        catch (e) { log(`  request failed (${e.message}); will retry`); }
      } else {
        log(`  validator unreachable and the owner program exposes no undelegation-request instruction; ` +
            `state is safe at the last L1 commit, waiting for the validator`);
      }
      return;
    }
    if (!acct.undelegate) { log(`  no cooperative undelegate wired for this account, skipping`); return; }
    if (dryRun) { log(`  DRY_RUN, would send cooperative undelegate`); return; }
    try {
      const sig = await withTimeout(acct.undelegate(er), 30_000);
      log(`  undelegated ${sig}`);
      await sleep(8_000);
      const after = await l1.getAccountInfo(acct.address).catch(() => null);
      log(after && !after.owner.equals(DELEGATION_PROGRAM_ID) ? `  HOME ✓` : `  still delegated, will retry`);
    } catch (e) {
      log(`  undelegate failed (${e.message}); will retry`);
    }
  }

  log(`er-guard up, ER ${erUrl}, ${accounts.length} account(s), idle ${idleMs}ms lifetime ${maxLifetimeMs}ms${dryRun ? " [DRY_RUN]" : ""}`);
  for (const a of accounts) log(`  watch ${a.address}${a.label ? `  (${a.label})` : ""}`);
  do {
    // A connection or token failure must not skip the account checks: the L1
    // side still detects delegation, and the probe failure feeds the stall
    // counter so the recovery path eventually fires.
    let ok = false;
    try {
      await ensureEr();
      try { await withTimeout(er.getVersion(), 8_000); ok = true; } catch { ok = false; }
    } catch (e) {
      log(`connection/token error: ${e.message}`);
    }
    stallFails = ok ? 0 : stallFails + 1;
    if (!ok) log(`validator probe failed (${stallFails}/${stallProbes}) ${erUrl}`);
    for (const a of accounts) {
      try { await check(a); } catch (e) { log(`check error ${short(String(a.address))}: ${e.message}`); }
    }
    if (!once) await sleep(pollMs);
  } while (!once);
}

export { edSignerFor };
