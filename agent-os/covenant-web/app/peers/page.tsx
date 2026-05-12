"use client";

import { useCallback, useState } from "react";
import { api } from "@/lib/api";
import { short, time } from "@/lib/audit";
import { usePoll } from "@/lib/usePoll";
import { PageHeader } from "../components/PageHeader";

type Status = "" | "live" | "revoked";

async function loadPeers(prefix: string, status: Status) {
  return api.listPeers(50, prefix || undefined, status || undefined);
}

export default function PeersPage() {
  const [prefix, setPrefix] = useState("");
  const [status, setStatus] = useState<Status>("");
  const fetcher = useCallback(() => loadPeers(prefix, status), [prefix, status]);
  const { data, error, lastSyncMs, refresh } = usePoll(fetcher, 4000);
  const [revoking, setRevoking] = useState<string | null>(null);
  const [rotating, setRotating] = useState(false);
  const [rotatedToken, setRotatedToken] = useState<string | null>(null);
  const [errMsg, setErrMsg] = useState<string | null>(null);

  const peers = data?.peers ?? [];
  const operatorPubkey = data?.operator_pubkey_b58 ?? "";
  const truncated = data?.truncated === true;
  const active = peers.filter((p) => p.revoked_at === null);

  async function onRotate() {
    if (typeof window !== "undefined" && !window.confirm("Rotate the operator token? Old token is revoked and any shell holding it must re-read peers/operator.token.")) {
      return;
    }
    setRotating(true);
    setErrMsg(null);
    try {
      const r = await api.rotateOperatorToken();
      if (r.kind === "operator_token_rotated") {
        setRotatedToken(r.token_b58);
        refresh();
      } else {
        setErrMsg(r.message);
      }
    } catch (e) {
      setErrMsg(e instanceof Error ? e.message : String(e));
    } finally {
      setRotating(false);
    }
  }

  async function onRevoke(tokenPrefix: string, display: string) {
    if (typeof window !== "undefined" && !window.confirm(`Revoke ${display} (token ${tokenPrefix}...)? This is irreversible.`)) return;
    setRevoking(tokenPrefix);
    setErrMsg(null);
    try {
      const r = await api.revokePeer(tokenPrefix);
      if (r.kind === "error") setErrMsg(r.message);
      else if (r.outcome.type === "ambiguous") {
        setErrMsg(`prefix ${tokenPrefix} matched multiple peers; use a longer prefix.`);
      } else if (r.outcome.type === "not_found") {
        setErrMsg(`prefix ${tokenPrefix} matched no peers`);
      }
      refresh();
    } catch (e) {
      setErrMsg(e instanceof Error ? e.message : String(e));
    } finally {
      setRevoking(null);
    }
  }

  return (
    <>
      <PageHeader
        eyebrow="identity mesh"
        title="Peers"
        subhead="Trusted identities on this daemon. Each peer carries a signing key and a bearer token; revocations are preserved, not deleted."
        syncMs={lastSyncMs}
        error={error}
        right={
          <button type="button" className="btn" onClick={onRotate} disabled={rotating}>
            {rotating ? "rotating" : "rotate operator token"}
          </button>
        }
      />

      {rotatedToken && (
        <article className="token-card">
          <p className="eyebrow">new operator token</p>
          <input readOnly value={rotatedToken} onFocus={(e) => e.currentTarget.select()} />
          <div className="actions">
            <button type="button" className="btn" onClick={() => navigator.clipboard?.writeText(rotatedToken)}>copy</button>
            <button type="button" className="btn ghost" onClick={() => setRotatedToken(null)}>dismiss</button>
          </div>
        </article>
      )}

      {errMsg && <pre className="result compact error">{errMsg}</pre>}

      <section className="filter-row panel">
        <div className="filter-group">
          <p className="eyebrow">filter</p>
          <div className="filters">
            <input
              value={prefix}
              onChange={(e) => setPrefix(e.target.value)}
              placeholder="filter by public-key prefix"
            />
            <select value={status} onChange={(e) => setStatus(e.target.value as Status)}>
              <option value="">all</option>
              <option value="live">live</option>
              <option value="revoked">revoked</option>
            </select>
          </div>
        </div>
        <div className="filter-stats">
          <span><strong>{active.length}</strong> live</span>
          <span><strong>{peers.length - active.length}</strong> revoked</span>
        </div>
      </section>

      {peers.length === 0 ? (
        <p className="empty">No peers match this filter.</p>
      ) : (
        <div className="peer-grid">
          {peers.map((peer) => {
            const isSelf = operatorPubkey !== "" && peer.agent_id.pubkey === operatorPubkey;
            const live = peer.revoked_at === null;
            return (
              <article key={`${peer.token_prefix}:${peer.registered_at}`} className={live ? "peer live" : "peer revoked"}>
                <div className="head">
                  <div className="avatar">{peer.agent_id.display.slice(0, 1).toUpperCase()}</div>
                  <div>
                    <strong>{peer.agent_id.display}</strong>
                    <p className="text-mono">{short(peer.agent_id.pubkey, 28)}</p>
                  </div>
                </div>
                <dl>
                  <div>
                    <dt>token</dt>
                    <dd className="text-mono">{peer.token_prefix}…</dd>
                  </div>
                  <div>
                    <dt>registered</dt>
                    <dd>{time(peer.registered_at)}</dd>
                  </div>
                </dl>
                <footer>
                  <span className={`badge ${live ? "live" : "revoked"}`}>
                    {live ? (isSelf ? "operator" : "live") : `revoked · ${time(peer.revoked_at ?? 0)}`}
                  </span>
                  {live && !isSelf && (
                    <button
                      type="button"
                      className="btn link"
                      onClick={() => onRevoke(peer.token_prefix, peer.agent_id.display)}
                      disabled={revoking === peer.token_prefix}
                    >
                      {revoking === peer.token_prefix ? "revoking" : "revoke"}
                    </button>
                  )}
                </footer>
              </article>
            );
          })}
        </div>
      )}

      {truncated && <p className="text-muted text-mono" style={{ marginTop: 12 }}>Showing first {peers.length}. Narrow with a prefix to see more.</p>}

      <style jsx>{`
        .token-card {
          margin-bottom: 22px;
          padding: 18px 22px;
          border: 1px solid var(--border);
          border-radius: 8px;
          background: var(--panel);
        }

        .token-card .eyebrow {
          margin: 0 0 10px;
        }

        .token-card input {
          width: 100%;
          font-family: var(--font-mono);
          font-size: 12px;
        }

        .token-card .actions {
          display: flex;
          gap: 8px;
          margin-top: 10px;
        }

        .filter-row.panel {
          display: flex;
          align-items: center;
          justify-content: space-between;
          gap: 18px;
          padding: 18px 22px;
          margin-bottom: 22px;
        }

        .filter-row .eyebrow {
          margin: 0 0 8px;
        }

        .filters {
          display: flex;
          gap: 8px;
        }

        .filters input {
          width: 240px;
        }

        .filter-stats {
          display: flex;
          gap: 18px;
          font-family: var(--font-mono);
          font-size: 12px;
          color: var(--muted);
        }

        .filter-stats strong {
          color: var(--fg);
          font-weight: 500;
        }

        .peer-grid {
          display: grid;
          grid-template-columns: repeat(auto-fill, minmax(260px, 1fr));
          gap: 12px;
        }

        .peer {
          display: grid;
          gap: 14px;
          padding: 16px 18px;
          border: 1px solid var(--border);
          border-radius: 8px;
          background: var(--panel);
        }

        .peer.revoked {
          border-color: var(--border-soft);
        }

        .peer .head {
          display: flex;
          align-items: center;
          gap: 12px;
        }

        .peer .avatar {
          display: grid;
          place-items: center;
          width: 32px;
          height: 32px;
          border: 1px solid var(--border);
          border-radius: 6px;
          color: var(--dim);
          font-family: var(--font-mono);
          font-size: 12px;
        }

        .peer strong {
          display: block;
          font-size: 14px;
        }

        .peer .head p {
          margin: 3px 0 0;
          font-size: 11px;
          color: var(--muted);
        }

        .peer dl {
          display: grid;
          grid-template-columns: repeat(2, 1fr);
          gap: 10px;
          margin: 0;
        }

        .peer dt {
          color: var(--muted);
          font-family: var(--font-mono);
          font-size: 10px;
          letter-spacing: 0.24em;
          text-transform: uppercase;
        }

        .peer dd {
          margin: 4px 0 0;
          font-size: 12px;
        }

        .peer footer {
          display: flex;
          align-items: center;
          justify-content: space-between;
          padding-top: 10px;
          border-top: 1px solid var(--border-soft);
        }

        .badge {
          font-family: var(--font-mono);
          font-size: 10px;
          letter-spacing: 0.18em;
          text-transform: uppercase;
          color: var(--dim);
        }

        .badge.live {
          color: var(--fg);
        }
      `}</style>
    </>
  );
}
