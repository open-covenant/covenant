"use client";

import Link from "next/link";
import { useCallback, useState, type FormEvent } from "react";
import { api } from "@/lib/api";
import {
  auditDetail,
  auditTone,
  eventIntentId,
  isReviewEvent,
  short,
  time,
  AUDIT_KIND_LABELS,
} from "@/lib/audit";
import { usePoll } from "@/lib/usePoll";
import { PageHeader } from "./components/PageHeader";

type OverviewSnapshot = Awaited<ReturnType<typeof loadOverview>>;

async function loadOverview() {
  const [audit, peers, caps, memory] = await Promise.all([
    api.recentAudit(40),
    api.listPeers(50),
    api.recentCapabilities(50),
    api.recentMemory(50),
  ]);
  return { audit, peers, caps, memory };
}

export default function OverviewPage() {
  const { data, error, lastSyncMs, refresh } = usePoll<OverviewSnapshot>(loadOverview, 3000);
  const [intent, setIntent] = useState("");
  const [dispatching, setDispatching] = useState(false);
  const [lastResult, setLastResult] = useState<string | null>(null);
  const [lastError, setLastError] = useState<string | null>(null);
  const [verifying, setVerifying] = useState(false);
  const [verifyMsg, setVerifyMsg] = useState<string | null>(null);

  const onDispatch = useCallback(
    async (e: FormEvent) => {
      e.preventDefault();
      if (!intent) return;
      setDispatching(true);
      setLastError(null);
      try {
        const r = await api.submitIntent(intent);
        if (r.kind === "intent_result") {
          setLastResult(r.text);
        } else {
          setLastError(r.message);
        }
        setIntent("");
        await refresh();
      } catch (e) {
        setLastError(e instanceof Error ? e.message : String(e));
      } finally {
        setDispatching(false);
      }
    },
    [intent, refresh],
  );

  const onVerify = useCallback(async () => {
    setVerifying(true);
    setVerifyMsg(null);
    try {
      const r = await api.verifyAudit();
      setVerifyMsg(
        r.report.valid
          ? `chain valid · ${r.report.events} events / ${r.report.anchors} anchors · root ${short(r.report.root_hash_hex, 14)}`
          : `chain INVALID · ${r.report.failures.length} failures`,
      );
    } catch (e) {
      setVerifyMsg(`verify failed: ${e instanceof Error ? e.message : String(e)}`);
    } finally {
      setVerifying(false);
    }
  }, []);

  const events = data?.audit.events ?? [];
  const recent = events.slice().reverse();
  const reviewRows = recent.filter(isReviewEvent).slice(0, 4);
  const peers = data?.peers.peers ?? [];
  const livePeers = peers.filter((p) => p.revoked_at === null);
  const caps = data?.caps.capabilities ?? [];
  const memory = data?.memory.records ?? [];
  const lastEvent = recent[0] ?? null;

  return (
    <>
      <PageHeader
        eyebrow="local control plane"
        title="Overview"
        subhead="Dispatch intents, manage capabilities, and verify audit integrity. Everything is signed and hash-chained on this machine."
        syncMs={lastSyncMs}
        error={error}
        right={
          <button type="button" className="btn" onClick={onVerify} disabled={verifying}>
            {verifying ? "verifying" : "verify chain"}
          </button>
        }
      />

      {verifyMsg && (
        <pre className={`result compact ${verifyMsg.includes("INVALID") || verifyMsg.includes("failed") ? "error" : ""}`}>
          {verifyMsg}
        </pre>
      )}

      <section className="dispatch-card">
        <form onSubmit={onDispatch}>
          <div className="row">
            <p className="eyebrow">dispatch intent</p>
            <span className="text-muted text-mono kbd-hint">⌘K to open the palette</span>
          </div>
          <textarea
            value={intent}
            onChange={(e) => setIntent(e.target.value)}
            placeholder="What should your agents do?"
            rows={2}
          />
          <div className="actions">
            <button type="submit" className="btn primary" disabled={dispatching || !intent}>
              {dispatching ? "dispatching" : "dispatch"}
            </button>
            {lastResult && <span className="result-line">→ {lastResult}</span>}
            {lastError && <span className="result-line error">⚠ {lastError}</span>}
          </div>
        </form>
      </section>

      <section className="metric-row">
        <article className="metric">
          <span className="eyebrow">live peers</span>
          <span className="value">{livePeers.length}</span>
          <span className="caption">
            {peers.length - livePeers.length} revoked
          </span>
        </article>
        <article className="metric">
          <span className="eyebrow">capabilities</span>
          <span className="value">{caps.length}</span>
          <span className="caption">active grants</span>
        </article>
        <article className="metric">
          <span className="eyebrow">memory records</span>
          <span className="value">{memory.length}</span>
          <span className="caption">
            {memory.filter((m) => m.tier === "working").length} working · {memory.filter((m) => m.tier === "episodic").length} episodic · {memory.filter((m) => m.tier === "longterm").length} long-term
          </span>
        </article>
        <article className="metric">
          <span className="eyebrow">last event</span>
          <span className="value small">
            {lastEvent ? AUDIT_KIND_LABELS[lastEvent.kind.type] : "—"}
          </span>
          <span className="caption">
            {lastEvent ? `${time(lastEvent.timestamp_ms)} · ${short(lastEvent.id, 8)}` : "no events yet"}
          </span>
        </article>
      </section>

      <section className="split-2">
        <div className="panel">
          <div className="panel-head">
            <div>
              <p className="eyebrow">activity</p>
              <h2>
                Recent audit events <span className="count">{recent.length}</span>
              </h2>
            </div>
            <Link className="btn ghost" href="/audit">
              all audit
            </Link>
          </div>
          {recent.length === 0 ? (
            <p className="empty">Nothing here yet. Your activity will appear as it happens.</p>
          ) : (
            <div className="records">
              {recent.slice(0, 8).map((event) => {
                const intentId = eventIntentId(event);
                const RowInner = (
                  <>
                    <div className="ts">
                      {time(event.timestamp_ms)}
                      <em>{AUDIT_KIND_LABELS[event.kind.type]}</em>
                    </div>
                    <div className="body">
                      <strong>{event.issuer.display}</strong>
                      <p>{auditDetail(event)}</p>
                    </div>
                  </>
                );
                if (intentId) {
                  return (
                    <Link
                      key={event.id}
                      href={`/intents/${intentId}`}
                      className={`record clickable tone-${auditTone(event)} fade-up`}
                    >
                      {RowInner}
                    </Link>
                  );
                }
                return (
                  <article key={event.id} className={`record tone-${auditTone(event)} fade-up`}>
                    {RowInner}
                  </article>
                );
              })}
            </div>
          )}
        </div>

        <div className="panel">
          <div className="panel-head">
            <div>
              <p className="eyebrow">review</p>
              <h2>
                Needs operator attention <span className="count">{reviewRows.length}</span>
              </h2>
            </div>
          </div>
          {reviewRows.length === 0 ? (
            <p className="empty">All clear. Nothing needs your attention.</p>
          ) : (
            <div className="records">
              {reviewRows.map((event) => (
                <article key={event.id} className={`record tone-${auditTone(event)}`}>
                  <div className="ts">
                    {time(event.timestamp_ms)}
                    <em>{AUDIT_KIND_LABELS[event.kind.type]}</em>
                  </div>
                  <div className="body">
                    <strong>{event.issuer.display}</strong>
                    <p>{auditDetail(event)}</p>
                  </div>
                </article>
              ))}
            </div>
          )}
        </div>
      </section>

      <style jsx>{`
        .dispatch-card {
          margin: 22px 0;
          padding: 22px;
          border: 1px solid var(--border);
          border-radius: 8px;
          background: var(--panel);
        }

        .dispatch-card form {
          display: grid;
          gap: 12px;
        }

        .dispatch-card .row {
          display: flex;
          align-items: center;
          justify-content: space-between;
          gap: 12px;
        }

        .dispatch-card textarea {
          min-height: 60px;
        }

        .dispatch-card .actions {
          display: flex;
          align-items: center;
          gap: 14px;
          flex-wrap: wrap;
        }

        .kbd-hint {
          font-size: 11px;
          letter-spacing: 0.08em;
        }

        .result-line {
          color: var(--dim);
          font-family: var(--font-mono);
          font-size: 12.5px;
          flex: 1;
          min-width: 0;
          overflow: hidden;
          text-overflow: ellipsis;
          white-space: nowrap;
        }

        .result-line.error {
          color: #fafafa;
        }

        .metric-row {
          margin-bottom: 22px;
        }
      `}</style>
    </>
  );
}
