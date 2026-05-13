"use client";

import Link from "next/link";
import { useCallback, useState, type FormEvent } from "react";
import { api } from "@/lib/api";
import { formatRelative, formatTimestamp } from "@/lib/format";
import { eventLabel, isReviewWorthy, memoryTierLabel } from "@/lib/labels";
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
  const [verifyOk, setVerifyOk] = useState<boolean>(true);

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
      setVerifyOk(r.report.valid);
      setVerifyMsg(
        r.report.valid
          ? `Activity log verified. ${r.report.events} signed steps, all intact.`
          : `Activity log tampered — ${r.report.failures.length} ${
              r.report.failures.length === 1 ? "failure" : "failures"
            } detected.`,
      );
    } catch (e) {
      setVerifyOk(false);
      setVerifyMsg(`Couldn't verify: ${e instanceof Error ? e.message : String(e)}`);
    } finally {
      setVerifying(false);
    }
  }, []);

  const events = data?.audit.events ?? [];
  const recent = events.slice().reverse();
  const reviewRows = (() => {
    const seen = new Set<string>();
    const out = [];
    for (const event of recent) {
      if (!isReviewWorthy(event)) continue;
      const dedupeKey =
        event.kind.type === "capability_check"
          ? `capability_check:${event.kind.agent_id}:${event.kind.required_actions.join(",")}`
          : event.kind.type === "authentication_failed"
            ? `auth:${event.kind.transport}:${event.kind.reason}`
            : `${event.kind.type}:${event.id}`;
      if (seen.has(dedupeKey)) continue;
      seen.add(dedupeKey);
      out.push(event);
      if (out.length >= 4) break;
    }
    return out;
  })();
  const peers = data?.peers.peers ?? [];
  const livePeers = peers.filter((p) => p.revoked_at === null);
  const caps = data?.caps.capabilities ?? [];
  const memory = data?.memory.records ?? [];
  const lastEvent = recent[0] ?? null;
  const lastLabel = lastEvent ? eventLabel(lastEvent) : null;

  return (
    <>
      <PageHeader
        eyebrow="local control plane"
        title="Overview"
        subhead="Send tasks to your agents, manage their permissions, and check that the activity log is intact. Everything happens on this machine."
        syncMs={lastSyncMs}
        error={error}
        right={
          <button type="button" className="btn" onClick={onVerify} disabled={verifying}>
            {verifying ? "Verifying" : "Verify activity log"}
          </button>
        }
      />

      {verifyMsg && (
        <pre className={`result compact ${verifyOk ? "" : "error"}`}>{verifyMsg}</pre>
      )}

      <section className="dispatch-card">
        <form onSubmit={onDispatch}>
          <div className="row">
            <p className="eyebrow">send a task</p>
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
              {dispatching ? "Sending" : "Send"}
            </button>
            {lastResult && <span className="result-line">{lastResult}</span>}
            {lastError && <span className="result-line error">{lastError}</span>}
          </div>
        </form>
      </section>

      <section className="metric-row">
        <article className="metric">
          <span className="eyebrow">connected agents</span>
          <span className="value">{livePeers.length}</span>
          <span className="caption">
            {peers.length - livePeers.length} revoked
          </span>
        </article>
        <article className="metric">
          <span className="eyebrow">permissions</span>
          <span className="value">{caps.length}</span>
          <span className="caption">currently granted</span>
        </article>
        <article className="metric">
          <span className="eyebrow">memories</span>
          <span className="value">{memory.length}</span>
          <span className="caption">
            {memory.filter((m) => m.tier === "working").length} {memoryTierLabel("working").toLowerCase()} ·{" "}
            {memory.filter((m) => m.tier === "episodic").length} {memoryTierLabel("episodic").toLowerCase()} ·{" "}
            {memory.filter((m) => m.tier === "longterm").length} {memoryTierLabel("longterm").toLowerCase()}
          </span>
        </article>
        <article className="metric">
          <span className="eyebrow">last activity</span>
          <span className="value small">{lastLabel ? lastLabel.headline : "—"}</span>
          <span className="caption">
            {lastEvent ? formatRelative(lastEvent.timestamp_ms) : "nothing yet"}
          </span>
        </article>
      </section>

      <section className="split-2">
        <div className="panel">
          <div className="panel-head">
            <div>
              <p className="eyebrow">recent activity</p>
              <h2>
                What&apos;s been happening <span className="count">{recent.length}</span>
              </h2>
            </div>
            <Link className="btn ghost" href="/audit">
              see all
            </Link>
          </div>
          {recent.length === 0 ? (
            <p className="empty">Nothing here yet. Your activity will appear as it happens.</p>
          ) : (
            <div className="records">
              {recent.slice(0, 8).map((event) => {
                const label = eventLabel(event);
                const RowInner = (
                  <>
                    <div className="ts">
                      {formatTimestamp(event.timestamp_ms)}
                      <em>{label.headline}</em>
                    </div>
                    <div className="body">
                      <strong>{event.issuer.display}</strong>
                      <p>{label.body}</p>
                    </div>
                  </>
                );
                if (label.intentId) {
                  return (
                    <Link
                      key={event.id}
                      href={`/intents/${label.intentId}`}
                      className={`record clickable tone-${label.tone} fade-up`}
                    >
                      {RowInner}
                    </Link>
                  );
                }
                return (
                  <article key={event.id} className={`record tone-${label.tone} fade-up`}>
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
              <p className="eyebrow">attention</p>
              <h2>
                Needs your review <span className="count">{reviewRows.length}</span>
              </h2>
            </div>
          </div>
          {reviewRows.length === 0 ? (
            <p className="empty">All clear. Nothing needs your attention.</p>
          ) : (
            <div className="records">
              {reviewRows.map((event) => {
                const label = eventLabel(event);
                return (
                  <article key={event.id} className={`record tone-${label.tone}`}>
                    <div className="ts">
                      {formatTimestamp(event.timestamp_ms)}
                      <em>{label.headline}</em>
                    </div>
                    <div className="body">
                      <strong>{event.issuer.display}</strong>
                      <p>{label.body}</p>
                    </div>
                  </article>
                );
              })}
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
