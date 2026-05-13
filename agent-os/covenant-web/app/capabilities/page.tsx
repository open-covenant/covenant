"use client";

import { useCallback, useMemo, useState, type FormEvent } from "react";
import { api } from "@/lib/api";
import { useDeveloperMode } from "@/lib/developerMode";
import {
  PEER_LOOKUP_LIMIT,
  expandA2aAction,
  formatExpandError,
  peerPrefixToLookup,
} from "@/lib/expand";
import { formatScope, shortHash } from "@/lib/format";
import { permissionLabel, type PermissionMeta } from "@/lib/labels";
import { usePoll } from "@/lib/usePoll";
import { PageHeader } from "../components/PageHeader";

async function loadCapabilities() {
  return api.recentCapabilities(60);
}

const SUGGESTIONS: Array<{ action: string; meta: PermissionMeta }> = [
  { action: "memory.write", meta: permissionLabel("memory.write") },
  { action: "tool.web_search", meta: permissionLabel("tool.web_search") },
  { action: "tool.summarize", meta: permissionLabel("tool.summarize") },
  { action: "agent.spawn", meta: permissionLabel("agent.spawn") },
];

export default function PermissionsPage() {
  const { data, error, lastSyncMs, refresh } = usePoll(loadCapabilities, 4000);
  const [devMode] = useDeveloperMode();
  const [action, setAction] = useState("");
  const [info, setInfo] = useState<string | null>(null);
  const [errMsg, setErrMsg] = useState<string | null>(null);
  const [revealedSig, setRevealedSig] = useState<string | null>(null);

  const onGrant = useCallback(
    async (e: FormEvent | null, override?: string) => {
      e?.preventDefault();
      const target = override ?? action;
      if (!target) return;
      setErrMsg(null);
      setInfo(null);

      let toGrant = target;
      const prefix = peerPrefixToLookup(target);
      if (prefix !== null) {
        try {
          const peers = await api.listPeers(PEER_LOOKUP_LIMIT, prefix);
          const result = expandA2aAction(target, peers.peers);
          if (!result.ok) {
            setErrMsg(formatExpandError(result.error));
            return;
          }
          if (result.value.kind === "rewritten") {
            toGrant = result.value.full;
            setInfo(`Resolved ${prefix} to ${result.value.full}.`);
          }
        } catch (e) {
          setErrMsg(e instanceof Error ? e.message : String(e));
          return;
        }
      }

      try {
        await api.grantCapability(toGrant);
        setAction("");
        setInfo(`Granted: ${permissionLabel(toGrant).title.toLowerCase()}.`);
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
  const preview = useMemo(() => (action ? permissionLabel(action) : null), [action]);

  return (
    <>
      <PageHeader
        eyebrow="what your agents can do"
        title="Permissions"
        subhead="Decide what each agent is allowed to do on your behalf. Every permission is signed so it can't be silently changed."
        syncMs={lastSyncMs}
        error={error}
      />

      <section className="grant-card">
        <form onSubmit={(e) => onGrant(e)}>
          <p className="eyebrow">grant a new permission</p>
          <div className="row">
            <input
              value={action}
              onChange={(e) => setAction(e.target.value)}
              placeholder="Pick one below or type its name"
            />
            <button type="submit" className="btn primary" disabled={!action}>
              Grant
            </button>
          </div>
          {preview && (
            <p className="preview">
              <span className={`risk risk-${preview.risk}`}>{preview.risk} risk</span>
              <strong>{preview.title}</strong>
              <span className="desc">{preview.description}</span>
            </p>
          )}
          <div className="suggestions">
            <span className="text-muted">Common:</span>
            {SUGGESTIONS.map((s) => (
              <button
                type="button"
                key={s.action}
                className="suggest"
                onClick={() => {
                  setAction(s.action);
                }}
              >
                {s.meta.title}
              </button>
            ))}
          </div>
        </form>
      </section>

      {info && <pre className="result compact">{info}</pre>}
      {errMsg && <pre className="result compact error">{errMsg}</pre>}

      <section className="panel">
        <div className="panel-head">
          <div>
            <p className="eyebrow">currently granted</p>
            <h2>
              What your agents can do <span className="count">{caps.length}</span>
            </h2>
          </div>
        </div>

        {caps.length === 0 ? (
          <p className="empty">Nothing granted yet. Add a permission above to let your agents act.</p>
        ) : (
          <div className="perm-list">
            {caps.map((cap, idx) => {
              const meta = permissionLabel(cap.capability.action);
              const isRevealed = revealedSig === cap.signature;
              return (
                <article key={`${cap.signature}-${idx}`} className="perm-row">
                  <div className="perm-main">
                    <div className="title-row">
                      <span className={`risk risk-${meta.risk}`}>{meta.risk}</span>
                      <strong>{meta.title}</strong>
                      {devMode && <code className="dev-action">{cap.capability.action}</code>}
                    </div>
                    <p className="desc">{meta.description}</p>
                    <p className="meta-line">
                      Granted to <strong>{cap.capability.subject.display}</strong> by{" "}
                      <strong>{cap.capability.granted_by.display}</strong> · {formatScope(cap.capability.scope)}
                    </p>
                  </div>
                  <div className="perm-actions">
                    <button
                      type="button"
                      className="btn link"
                      onClick={() => setRevealedSig(isRevealed ? null : cap.signature)}
                    >
                      {isRevealed ? "hide signature" : "show signature"}
                    </button>
                    <button type="button" className="btn ghost" onClick={() => onRevoke(cap.signature)}>
                      Revoke
                    </button>
                  </div>
                  {isRevealed && (
                    <pre className="result compact perm-sig">
                      {shortHash(cap.signature, 64)}
                    </pre>
                  )}
                </article>
              );
            })}
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
          gap: 12px;
        }

        .grant-card .row {
          display: grid;
          grid-template-columns: 1fr auto;
          gap: 8px;
        }

        .preview {
          margin: 0;
          padding: 12px 14px;
          border: 1px solid var(--border-soft);
          border-radius: 6px;
          background: var(--bg);
          display: grid;
          gap: 4px;
        }

        .preview strong {
          font-size: 14px;
          color: var(--fg);
          font-weight: 500;
        }

        .preview .desc {
          font-size: 12.5px;
          color: var(--dim);
          line-height: 1.5;
        }

        .risk {
          display: inline-block;
          padding: 2px 8px;
          border: 1px solid var(--border);
          border-radius: 999px;
          font-family: var(--font-mono);
          font-size: 10px;
          letter-spacing: 0.16em;
          text-transform: uppercase;
          color: var(--muted);
          width: fit-content;
        }

        .risk-low {
          color: var(--dim);
        }

        .risk-med {
          color: #d4d4d4;
        }

        .risk-high {
          color: #fafafa;
          border-color: var(--faint);
        }

        .suggestions {
          display: flex;
          flex-wrap: wrap;
          align-items: center;
          gap: 8px;
          font-size: 11.5px;
        }

        .suggestions .text-muted {
          color: var(--muted);
          font-family: var(--font-mono);
          font-size: 10px;
          letter-spacing: 0.18em;
          text-transform: uppercase;
        }

        .suggest {
          padding: 4px 10px;
          border: 1px solid var(--border);
          border-radius: 999px;
          background: var(--bg);
          color: var(--dim);
          font-size: 12px;
          transition: border-color 120ms ease, color 120ms ease;
        }

        .suggest:hover {
          border-color: var(--fg);
          color: var(--fg);
        }

        .perm-list {
          display: grid;
        }

        .perm-row {
          display: grid;
          grid-template-columns: 1fr auto;
          gap: 18px;
          padding: 18px 22px;
          border-top: 1px solid var(--border-soft);
          align-items: start;
        }

        .perm-main {
          display: grid;
          gap: 6px;
          min-width: 0;
        }

        .title-row {
          display: flex;
          align-items: center;
          gap: 10px;
        }

        .title-row strong {
          color: var(--fg);
          font-size: 14.5px;
          font-weight: 500;
        }

        .perm-row .desc {
          margin: 0;
          color: var(--dim);
          font-size: 13px;
          line-height: 1.5;
        }

        .meta-line {
          margin: 0;
          color: var(--muted);
          font-size: 12px;
        }

        .meta-line strong {
          color: var(--dim);
          font-weight: 500;
        }

        .perm-actions {
          display: flex;
          gap: 8px;
          align-items: center;
          flex-wrap: wrap;
          justify-content: flex-end;
        }

        .perm-sig {
          grid-column: 1 / -1;
          margin: 8px 0 0;
        }

        .dev-action {
          padding: 2px 6px;
          border: 1px solid var(--border-soft);
          border-radius: 4px;
          background: var(--bg);
          color: var(--muted);
          font-family: var(--font-mono);
          font-size: 10.5px;
        }

        @media (max-width: 720px) {
          .perm-row {
            grid-template-columns: 1fr;
          }

          .perm-actions {
            justify-content: flex-start;
          }
        }
      `}</style>
    </>
  );
}
