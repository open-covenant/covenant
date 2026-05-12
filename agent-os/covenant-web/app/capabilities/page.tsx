"use client";

import { useCallback, useState, type FormEvent } from "react";
import { api } from "@/lib/api";
import { short } from "@/lib/audit";
import {
  PEER_LOOKUP_LIMIT,
  expandA2aAction,
  formatExpandError,
  peerPrefixToLookup,
} from "@/lib/expand";
import { usePoll } from "@/lib/usePoll";
import { PageHeader } from "../components/PageHeader";

async function loadCapabilities() {
  return api.recentCapabilities(60);
}

function scopeSummary(scope: unknown): string {
  if (scope == null) return "global";
  if (typeof scope !== "object") return String(scope);
  const json = JSON.stringify(scope);
  if (!json || json === "{}") return "global";
  return json;
}

export default function CapabilitiesPage() {
  const { data, error, lastSyncMs, refresh } = usePoll(loadCapabilities, 4000);
  const [action, setAction] = useState("");
  const [info, setInfo] = useState<string | null>(null);
  const [errMsg, setErrMsg] = useState<string | null>(null);

  const onGrant = useCallback(
    async (e: FormEvent) => {
      e.preventDefault();
      if (!action) return;
      setErrMsg(null);
      setInfo(null);

      let toGrant = action;
      const prefix = peerPrefixToLookup(action);
      if (prefix !== null) {
        try {
          const peers = await api.listPeers(PEER_LOOKUP_LIMIT, prefix);
          const result = expandA2aAction(action, peers.peers);
          if (!result.ok) {
            setErrMsg(formatExpandError(result.error));
            return;
          }
          if (result.value.kind === "rewritten") {
            toGrant = result.value.full;
            setInfo(`expanded ${prefix} → ${result.value.full}`);
          }
        } catch (e) {
          setErrMsg(e instanceof Error ? e.message : String(e));
          return;
        }
      }

      try {
        await api.grantCapability(toGrant);
        setAction("");
        refresh();
      } catch (e) {
        setErrMsg(e instanceof Error ? e.message : String(e));
      }
    },
    [action, refresh],
  );

  async function onRevoke(sig: string) {
    setErrMsg(null);
    try {
      await api.revokeCapability(sig);
      refresh();
    } catch (e) {
      setErrMsg(e instanceof Error ? e.message : String(e));
    }
  }

  const caps = data?.capabilities ?? [];

  return (
    <>
      <PageHeader
        eyebrow="signed grants"
        title="Capabilities"
        subhead="Every privileged action requires a signed capability. Grants and revocations are recorded in the audit log."
        syncMs={lastSyncMs}
        error={error}
      />

      <section className="grant-card">
        <form onSubmit={onGrant}>
          <p className="eyebrow">grant capability</p>
          <div className="row">
            <input
              value={action}
              onChange={(e) => setAction(e.target.value)}
              placeholder="memory.write · tool.web_search · agent.spawn"
            />
            <button type="submit" className="btn primary" disabled={!action}>
              grant
            </button>
          </div>
          <p className="text-muted text-mono hint">
            Namespaces: <em>intent</em> · <em>memory</em> · <em>identity</em> · <em>tool</em> · <em>agent</em>
          </p>
        </form>
      </section>

      {info && <pre className="result compact">{info}</pre>}
      {errMsg && <pre className="result compact error">{errMsg}</pre>}

      <section className="panel">
        <div className="panel-head">
          <div>
            <p className="eyebrow">granted</p>
            <h2>
              Active capabilities <span className="count">{caps.length}</span>
            </h2>
          </div>
        </div>

        {caps.length === 0 ? (
          <p className="empty">Nothing granted yet.</p>
        ) : (
          <div className="records">
            {caps.map((cap, idx) => (
              <article key={`${cap.signature}-${idx}`} className="record">
                <div className="ts">
                  <span>{cap.capability.subject.display}</span>
                  <em>signed by {cap.capability.granted_by.display}</em>
                </div>
                <div className="body">
                  <strong>{cap.capability.action}</strong>
                  <p className="text-mono">scope · {scopeSummary(cap.capability.scope)}</p>
                  <p className="text-mono text-muted">sig {short(cap.signature, 22)}</p>
                </div>
                <div className="right">
                  <button type="button" className="btn link" onClick={() => onRevoke(cap.signature)}>
                    revoke
                  </button>
                </div>
              </article>
            ))}
          </div>
        )}
      </section>

      <style jsx>{`
        .grant-card {
          margin-bottom: 22px;
          padding: 22px;
          border: 1px solid var(--border);
          border-radius: 8px;
          background: var(--panel);
        }

        .grant-card .eyebrow {
          margin: 0 0 12px;
        }

        .grant-card form {
          display: grid;
          gap: 10px;
        }

        .grant-card .row {
          display: grid;
          grid-template-columns: 1fr auto;
          gap: 8px;
        }

        .grant-card .hint {
          margin: 0;
          font-size: 11px;
          letter-spacing: 0.04em;
        }

        .grant-card .hint em {
          font-style: normal;
          color: var(--dim);
        }
      `}</style>
    </>
  );
}
