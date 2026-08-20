// Covenant "verified enclave" badge for Loofta Pay.
//
// Loofta settles inside MagicBlock Private Ephemeral Rollups (Intel TDX
// enclaves). Covenant DCAP-verifies those enclaves on mainnet and keeps a
// verified-ER attestation live in the Solana Attestation Service. Given the ER a
// payment settled on, this resolves that attestation (read-only, one RPC read of
// the attestation and its credential, no keys) and renders a drop-in badge for a
// checkout or receipt.
//
// The resolve runs server-side (verified-er reads Solana + SAS through node);
// the renderers are pure string builders, so the status can be shipped to the
// client and drawn anywhere. It fails closed: a lapsed, missing, or
// wrong-signer attestation renders as "not verified", never a false green.

import { Connection } from "@solana/web3.js";
import {
  resolveVerifiedEr,
  pickVerifiedEr,
  deriveAttestationPda,
} from "@covenant-org/verified-er";

const asConnection = ({ connection, rpcUrl, timeoutMs = 10_000 }) => {
  if (connection) return connection;
  if (rpcUrl) {
    return new Connection(rpcUrl, {
      commitment: "confirmed",
      fetch: (url, opts = {}) => fetch(url, { ...opts, signal: AbortSignal.timeout(timeoutMs) }),
    });
  }
  throw new Error("pass a `connection` or `rpcUrl`");
};

const short = (s, n = 4) =>
  s && s.length > n * 2 + 1 ? `${s.slice(0, n)}…${s.slice(-n)}` : (s ?? "");
const iso = (unixSecs) => (unixSecs ? new Date(unixSecs * 1000).toISOString() : null);

/** Resolve the Covenant attestation for the validator a payment settled on and
 *  normalize it into badge data. */
export async function verifyEnclave({ connection, rpcUrl, validator }) {
  if (!validator) throw new Error("verifyEnclave needs the settlement `validator` (ER identity)");
  try {
    const res = await resolveVerifiedEr(asConnection({ connection, rpcUrl }), validator);
    return toBadgeData(validator, res);
  } catch (e) {
    return toBadgeData(validator, { verified: false, reason: `could not verify (${e.message ?? "rpc error"})` });
  }
}

/** Route to a Covenant-verified ER and return both the endpoint to send to and
 *  the badge data, in one call. Use this when you let Covenant pick the enclave
 *  rather than pinning a validator. */
export async function routeAndVerify({ connection, rpcUrl }) {
  try {
    const { picked, routes } = await pickVerifiedEr(asConnection({ connection, rpcUrl }));
    return {
      endpoint: picked?.fqdn ?? null,
      validator: picked?.identity ?? null,
      badge: picked
        ? toBadgeData(picked.identity, picked.covenant)
        : toBadgeData(null, { verified: false, reason: "no verified ER available" }),
      routes,
    };
  } catch (e) {
    return {
      endpoint: null,
      validator: null,
      badge: toBadgeData(null, { verified: false, reason: `could not verify (${e.message ?? "rpc error"})` }),
      routes: [],
    };
  }
}

function toBadgeData(validator, res) {
  const att =
    res.attestation?.toBase58?.() ??
    (typeof res.attestation === "string" ? res.attestation : null) ??
    (validator ? deriveAttestationPda(validator).toBase58() : null);
  return {
    verified: Boolean(res.verified),
    reason: res.reason ?? null,
    validator: validator ?? null,
    tcbStatus: res.status ?? null,
    mrTd: res.mrTd ?? null,
    mrTdShort: short(res.mrTd, 6),
    verifiedAt: res.verifiedAt ?? null,
    verifiedAtIso: iso(res.verifiedAt),
    expiry: res.expiry ?? null,
    expiryIso: iso(res.expiry),
    attestation: att,
    explorerUrl: att ? `https://solscan.io/account/${att}` : null,
  };
}

const esc = (s) =>
  String(s ?? "").replace(/[&<>"']/g, (c) => (
    { "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" }[c]
  ));

const THEME = {
  light: { okBg: "#ECFDF3", okBd: "#ABEFC6", okFg: "#067647", offBg: "#FFFAEB", offBd: "#FEDF89", offFg: "#B54708", sub: "#475467", mono: "#101828", link: "#175CD3" },
  dark: { okBg: "#053321", okBd: "#085D3A", okFg: "#75E0A7", offBg: "#4E1D09", offBd: "#93370D", offFg: "#FEC84B", sub: "#98A2B3", mono: "#F5F5F5", link: "#84CAFF" },
};

const shield = (fg) =>
  `<svg width="14" height="14" viewBox="0 0 24 24" fill="none" aria-hidden="true" style="flex:0 0 auto"><path d="M12 2l7 3v6c0 4.5-3 8.3-7 9.5C8 19.3 5 15.5 5 11V5l7-3z" fill="${fg}" opacity="0.16"/><path d="M9.2 12.2l2 2 3.6-4" stroke="${fg}" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"/><path d="M12 2l7 3v6c0 4.5-3 8.3-7 9.5C8 19.3 5 15.5 5 11V5l7-3z" stroke="${fg}" stroke-width="1.5" stroke-linejoin="round"/></svg>`;

/** A self-contained, inline-styled badge. Drop the returned string into a
 *  checkout, receipt, or email. `compact` renders just the pill. */
export function badgeHtml(data, { theme = "light", compact = false } = {}) {
  const t = THEME[theme] ?? THEME.light;
  const ok = data.verified;
  const bg = ok ? t.okBg : t.offBg;
  const bd = ok ? t.okBd : t.offBd;
  const fg = ok ? t.okFg : t.offFg;
  const label = ok ? "Covenant-verified enclave" : "Enclave not verified";
  const font = "-apple-system,BlinkMacSystemFont,'Segoe UI',Roboto,Helvetica,Arial,sans-serif";

  const pill =
    `<span style="display:inline-flex;align-items:center;gap:6px;padding:4px 10px;` +
    `border:1px solid ${bd};background:${bg};color:${fg};border-radius:999px;` +
    `font:600 12px/1.2 ${font};white-space:nowrap">${shield(fg)}${esc(label)}</span>`;

  if (compact || !ok) {
    const why = ok ? "" : `<span style="font:400 11px/1.4 ${font};color:${t.sub};margin-left:8px">${esc(data.reason ?? "")}</span>`;
    return `<span style="display:inline-flex;align-items:center">${pill}${why}</span>`;
  }

  const row = (k, v, mono = false) =>
    `<div style="display:flex;justify-content:space-between;gap:16px;padding:2px 0">` +
    `<span style="font:400 11px/1.5 ${font};color:${t.sub}">${esc(k)}</span>` +
    `<span style="font:${mono ? "400 11px/1.5 ui-monospace,SFMono-Regular,Menlo,monospace" : `500 11px/1.5 ${font}`};color:${t.mono}">${esc(v)}</span></div>`;

  const link = data.explorerUrl
    ? `<a href="${esc(data.explorerUrl)}" target="_blank" rel="noopener" style="font:500 11px/1.5 ${font};color:${t.link};text-decoration:none">View attestation ↗</a>`
    : "";

  return (
    `<div style="display:inline-block;min-width:280px;max-width:360px;padding:12px;` +
    `border:1px solid ${bd};background:${bg};border-radius:12px">` +
    `<div style="margin-bottom:8px">${pill}</div>` +
    row("Attested TCB", data.tcbStatus ?? "unknown") +
    row("Enclave (mr_td)", data.mrTdShort || "n/a", true) +
    row("ER validator", short(data.validator, 4), true) +
    (data.verifiedAtIso ? row("Verified", data.verifiedAtIso.replace("T", " ").replace(/\..+/, " UTC")) : "") +
    `<div style="margin-top:8px">${link}</div>` +
    `</div>`
  );
}

/** One-line plain-text form for logs and non-HTML contexts. */
export function badgeText(data) {
  return data.verified
    ? `✓ Covenant-verified enclave  (TCB ${data.tcbStatus}, validator ${short(data.validator)}, verified ${data.verifiedAtIso})`
    : `✗ Enclave not verified  (${data.reason})`;
}
