"use client";

import Link from "next/link";
import { use, useEffect, useState } from "react";
import { api, type AuditEvent } from "@/lib/api";
import { eventsForIntent } from "@/lib/audit";
import { formatAgentId, formatTimestamp, shortHash } from "@/lib/format";
import { loadReply } from "@/lib/intentReplies";
import {
  AGENT_EVENT_PILL_LABELS,
  KIND_PILL_LABELS,
  agentEventLabel,
  eventLabel,
} from "@/lib/labels";
import { useIntentEventStream, type LiveAgentEvent } from "@/lib/useIntentEventStream";
import { usePoll } from "@/lib/usePoll";
import { BuildOutput } from "../../components/BuildOutput";
import { Markdown } from "../../components/Markdown";
import { PageHeader } from "../../components/PageHeader";

const DEMO_MODE = process.env.NEXT_PUBLIC_DEMO_MODE === "1";

function statusWord(status: string | undefined): string {
  switch (status) {
    case "completed":
    case "ok":
      return "Done";
    case "failed":
    case "error":
      return "Failed";
    case "running":
      return "Running";
    case "pending":
      return "Pending";
    default:
      return status ? status[0].toUpperCase() + status.slice(1) : "—";
  }
}

// Discriminated union for the unified trace timeline. Audit rows are the
// durable hash-chained record; live rows are SSE-pushed AgentEvents that
// have not yet been persisted (or won't be — reasoning is a wire-only
// slot today). Sort key is `timestampMs` so both sources interleave
// chronologically without dedupe — see useIntentEventStream for why a
// 1:1 audit/live dedupe is intentionally out of scope for this slice.
type TraceItem =
  | { source: "audit"; key: string; timestampMs: number; event: AuditEvent }
  | { source: "live"; key: string; timestampMs: number; event: LiveAgentEvent };

// Cap on rendered trace items per page. Long-running coding runs can
// emit hundreds of tool events; rendering them all without bound freezes
// the React reconciler on slow machines. Showing the most recent N keeps
// the page interactive — operators expand to the full list when they
// want to audit a finished run. Pick 200 because that matches the audit
// fetch `limit=200`; lifting the cap should also widen the audit fetch.
const RENDER_LIMIT_DEFAULT = 200;

export default function TaskTracePage(props: { params: Promise<{ id: string }> }) {
  const { id } = use(props.params);
  const [auditEvents, setAuditEvents] = useState<AuditEvent[] | null>(null);
  const [auditError, setAuditError] = useState<string | null>(null);
  const [auditSyncMs, setAuditSyncMs] = useState<number | null>(null);
  const { data: outcome } = usePoll(() => api.intentResult(id), 3000);
  const liveStream = useIntentEventStream(id);
  const [expanded, setExpanded] = useState<Set<string>>(new Set());
  const [showAllSteps, setShowAllSteps] = useState(false);
  const [reply, setReply] = useState<string | null>(null);

  // Audit fetch: durable historical context for events that landed
  // before this tab opened, plus the dispatch row (with its signed
  // result hash) that lands when an async run completes. Re-runs on
  // intent navigation (`id` change, since Next.js client-side nav does
  // not remount the page) and once the polled outcome flips into a
  // terminal status — without that second fetch, fresh runs would
  // never surface the `intent_dispatched` row and the signed-hash
  // badge would stay hidden until manual reload.
  const outcomeStatus = outcome?.status;
  const outcomeTerminal =
    outcomeStatus === "ok" ||
    outcomeStatus === "error" ||
    outcomeStatus === "completed" ||
    outcomeStatus === "failed" ||
    outcomeStatus === "ignored";
  useEffect(() => {
    let cancelled = false;
    api
      .recentAudit(200)
      .then((data) => {
        if (cancelled) return;
        setAuditEvents(data.events);
        setAuditSyncMs(Date.now());
        setAuditError(null);
      })
      .catch((e: unknown) => {
        if (cancelled) return;
        const message = e instanceof Error ? e.message : String(e);
        setAuditError(
          message.includes("Failed to fetch")
            ? "daemon unavailable at 127.0.0.1:8421"
            : message,
        );
      });
    return () => {
      cancelled = true;
    };
  }, [id, outcomeTerminal]);

  useEffect(() => {
    // eslint-disable-next-line react-hooks/set-state-in-effect
    setReply(loadReply(id));
  }, [id]);

  const error = auditError ?? liveStream.error;
  const lastSyncMs = auditSyncMs;
  const auditTrace = eventsForIntent(auditEvents ?? [], id);
  const dispatched = auditTrace.find((e) => e.kind.type === "intent_dispatched");
  const dispatchKind = dispatched?.kind.type === "intent_dispatched" ? dispatched.kind : null;

  const traceItems: TraceItem[] = [
    ...auditTrace.map<TraceItem>((event) => ({
      source: "audit",
      key: `audit:${event.id}`,
      timestampMs: event.timestamp_ms,
      event,
    })),
    ...liveStream.events.map<TraceItem>((event) => ({
      source: "live",
      key: `live:${event.seq}`,
      timestampMs: event.receivedMs,
      event,
    })),
  ].sort((a, b) => a.timestampMs - b.timestampMs);
  // Cap the render set so a 1000-step run does not freeze the page;
  // operators expand on demand. Take from the tail so the user sees what
  // is happening NOW (the same window a `tail -f` would surface), not
  // the first 200 events that may be ancient by the time they look.
  const overflow = Math.max(0, traceItems.length - RENDER_LIMIT_DEFAULT);
  const visibleTraceItems =
    !showAllSteps && overflow > 0
      ? traceItems.slice(traceItems.length - RENDER_LIMIT_DEFAULT)
      : traceItems;
  // While an async build is in flight the audit trace is still empty (the
  // dispatch row lands when the run finishes), so fall back to the polled
  // outcome for the title, agent, status, and reply body. The live SSE
  // stream fills in steps as they happen even before the dispatch row
  // commits.
  const running = outcome?.status === "running";
  const intentText = dispatchKind?.intent_text ?? outcome?.intent_text ?? null;
  const matchedAgent = dispatchKind?.matched_agent ?? outcome?.matched_agent ?? null;
  const status = outcome?.status ?? dispatchKind?.status;
  const replyText = (outcome?.text && outcome.text.length > 0 ? outcome.text : null) ?? reply;
  const totalDurationMs =
    traceItems.length > 1
      ? traceItems[traceItems.length - 1].timestampMs - traceItems[0].timestampMs
      : null;
  const isLoading = auditEvents === null && auditError === null;

  function toggle(id: string) {
    setExpanded((prev) => {
      const next = new Set(prev);
      if (next.has(id)) next.delete(id);
      else next.add(id);
      return next;
    });
  }

  return (
    <>
      <PageHeader
        eyebrow="task"
        title={intentText ? `“${intentText}”` : "Task"}
        subhead={
          running
            ? traceItems.length > 0
              ? `Watching live${matchedAgent ? ` · ${formatAgentId(matchedAgent)}` : ""} — each step lands the moment the sandbox emits it.`
              : `Building in the sandbox${matchedAgent ? ` · ${formatAgentId(matchedAgent)}` : ""}. Steps stream in as the agent works.`
            : dispatchKind
              ? dispatchKind.matched_agent
                ? `Ran by ${formatAgentId(dispatchKind.matched_agent)}. The result is signed (${shortHash(dispatchKind.result_hash_hex, 10)}) so it can’t be quietly changed.`
                : "No agent is set up to handle this kind of task. Covenant returned a default response."
              : "Loading the task’s steps…"
        }
        syncMs={lastSyncMs}
        error={error}
        right={
          <Link href="/intents" className="btn ghost">
            back to tasks
          </Link>
        }
      />

      <section className="trace-meta">
        <article className="meta-cell">
          <p className="eyebrow">steps</p>
          <strong>{traceItems.length}</strong>
        </article>
        <article className="meta-cell">
          <p className="eyebrow">took</p>
          <strong>{totalDurationMs == null ? "—" : `${totalDurationMs} ms`}</strong>
        </article>
        <article className="meta-cell">
          <p className="eyebrow">ran by</p>
          <strong>{formatAgentId(matchedAgent)}</strong>
        </article>
        <article className="meta-cell">
          <p className="eyebrow">status</p>
          <strong>{statusWord(status)}</strong>
        </article>
      </section>

      <section className="reply-panel">
        <div className="reply-head">
          <p className="eyebrow">reply</p>
          {dispatchKind && (
            <span className="hash">
              signed hash {shortHash(dispatchKind.result_hash_hex, 10)}
            </span>
          )}
        </div>
        <div className="reply-body">
          {running ? (
            <p className="empty">
              Building in the sandbox… the reply lands here when the run finishes.
            </p>
          ) : replyText ? (
            <Markdown>{replyText}</Markdown>
          ) : outcome === null ? (
            // Shared link to an evicted or unknown intent: the daemon
            // does not track this id (404 on /intents/:id/result) and we
            // have no per-tab fallback to show. Be explicit so a shared
            // URL does not look broken.
            <p className="empty">
              This task is no longer cached on the server. The full reply
              is only retained for a short retention window. The activity
              log keeps the signed result hash so the run can still be
              verified.
            </p>
          ) : (
            <p className="empty">
              The reply body isn&apos;t available in this tab. The activity log stores
              a signed hash of the reply, not the body itself, so a tab that
              didn&apos;t submit this task can&apos;t re-render it. Send the task
              again from the Overview to see the reply.
            </p>
          )}
        </div>
      </section>

      {outcome?.files && outcome.files.length > 0 && <BuildOutput files={outcome.files} />}

      {isLoading ? (
        <div className="panel">
          <p className="empty">Loading the task&apos;s steps…</p>
        </div>
      ) : running && traceItems.length === 0 ? (
        // Run is in flight but no event has arrived yet — short window
        // between submit and the first tool call. The SSE stream is open
        // (or reconnecting); the first frame will trigger a re-render
        // into the live-trace branch below.
        <div className="panel running-empty">
          <span className="pulse" aria-hidden="true" />
          <p className="empty">
            Waiting for the sandbox to emit its first step. The
            connection is live — steps appear the moment they happen.
          </p>
        </div>
      ) : traceItems.length === 0 ? (
        <div className="panel">
          <p className="empty">
            {DEMO_MODE
              ? "Can't find this task in the activity log. It may be older than the sandbox retention window — shared state resets periodically."
              : "Can't find this task in the local log. It may be older than what's cached on this machine."}
          </p>
        </div>
      ) : (
        <div className={`trace${running ? " trace-live" : ""}`}>
          {running && (
            // Header pip on the live timeline — operators want a single
            // visual cue that the events below are streaming in real
            // time, not a paused snapshot.
            <article className="trace-live-banner">
              <span className="pulse" aria-hidden="true" />
              <span>Live · streaming from the sandbox</span>
            </article>
          )}
          {overflow > 0 && (
            <article className="trace-overflow">
              <p>
                Showing the most recent {RENDER_LIMIT_DEFAULT} of{" "}
                {traceItems.length} steps.{" "}
                <button
                  type="button"
                  className="btn link"
                  onClick={() => setShowAllSteps((v) => !v)}
                >
                  {showAllSteps ? "collapse" : `show all ${traceItems.length}`}
                </button>
              </p>
            </article>
          )}
          {visibleTraceItems.map((item, idx) => {
            const isLast = idx === visibleTraceItems.length - 1;
            const isExpanded = expanded.has(item.key);
            const label =
              item.source === "audit"
                ? eventLabel(item.event)
                : agentEventLabel(item.event);
            const pill =
              item.source === "audit"
                ? KIND_PILL_LABELS[item.event.kind.type]
                : AGENT_EVENT_PILL_LABELS[item.event.type];
            return (
              <article key={item.key} className={`trace-step tone-${label.tone}`}>
                <div className="rail">
                  <span className="dot" />
                  {!isLast && <span className="line" />}
                </div>
                <div className="step-card">
                  <div className="step-head">
                    <span className="ts">
                      {formatTimestamp(item.timestampMs, { withSeconds: true })}
                    </span>
                    <span className="kind">{pill}</span>
                    {item.source === "live" && <span className="live">live</span>}
                    <button
                      type="button"
                      className="btn link"
                      onClick={() => toggle(item.key)}
                    >
                      {isExpanded ? "hide raw" : "raw json"}
                    </button>
                  </div>
                  <p className="headline">{label.headline}</p>
                  <p className="summary">{label.body}</p>
                  {isExpanded && (
                    <pre className="result compact">
                      {JSON.stringify(item.event, null, 2)}
                    </pre>
                  )}
                </div>
              </article>
            );
          })}
        </div>
      )}

      <style jsx>{`
        .trace-meta {
          display: grid;
          grid-template-columns: repeat(4, minmax(0, 1fr));
          gap: 12px;
          margin-bottom: 24px;
        }

        .meta-cell {
          padding: 14px 16px;
          border: 1px solid var(--border);
          border-radius: 8px;
          background: var(--panel);
        }

        .meta-cell .eyebrow {
          margin: 0 0 8px;
        }

        .meta-cell strong {
          display: block;
          color: #fafafa;
          font-family: var(--font-mono);
          font-size: 22px;
          font-weight: 400;
          letter-spacing: -0.01em;
        }

        @media (max-width: 800px) {
          .trace-meta {
            grid-template-columns: repeat(2, minmax(0, 1fr));
          }
        }

        .reply-panel {
          margin-bottom: 28px;
          border: 1px solid var(--border);
          border-radius: 8px;
          background: var(--panel);
          overflow: hidden;
        }

        .reply-panel .reply-head {
          display: flex;
          align-items: center;
          justify-content: space-between;
          gap: 12px;
          padding: 10px 16px;
          border-bottom: 1px solid var(--border);
          background: #0a0a0a;
        }

        .reply-panel .reply-head .eyebrow {
          margin: 0;
        }

        .reply-panel .reply-head .hash {
          color: var(--muted);
          font-family: var(--font-mono);
          font-size: 11px;
          letter-spacing: 0.04em;
        }

        .reply-panel .reply-body {
          padding: 14px 18px;
          max-height: 420px;
          overflow: auto;
        }

        .reply-panel .reply-body pre {
          margin: 0;
          color: var(--fg);
          font-family: var(--font-body);
          font-size: 14px;
          line-height: 1.55;
          white-space: pre-wrap;
          word-break: break-word;
        }

        .reply-panel .empty {
          margin: 0;
          color: var(--dim);
          font-size: 13px;
          line-height: 1.55;
          max-width: 70ch;
        }

        .trace {
          display: grid;
          gap: 0;
        }

        .trace-step {
          display: grid;
          grid-template-columns: 22px 1fr;
          gap: 12px;
        }

        .rail {
          position: relative;
          display: flex;
          justify-content: center;
          padding-top: 18px;
        }

        .rail .dot {
          position: relative;
          z-index: 2;
          width: 10px;
          height: 10px;
          border-radius: 999px;
          background: var(--dim);
          border: 2px solid var(--bg);
        }

        .tone-ok .rail .dot {
          background: #d4d4d4;
        }

        .tone-warn .rail .dot {
          background: #737373;
        }

        .tone-danger .rail .dot {
          background: #fafafa;
        }

        .rail .line {
          position: absolute;
          left: 50%;
          top: 28px;
          bottom: -2px;
          width: 1px;
          background: var(--border);
          transform: translateX(-50%);
        }

        .step-card {
          padding: 12px 16px 14px;
          margin-bottom: 8px;
          border: 1px solid var(--border-soft);
          border-radius: 8px;
          background: var(--panel);
        }

        .trace-overflow {
          margin-bottom: 12px;
          padding: 10px 14px;
          border: 1px dashed var(--border);
          border-radius: 6px;
          background: #0a0a0a;
        }

        .trace-overflow p {
          margin: 0;
          color: var(--muted);
          font-size: 12px;
        }

        .step-head {
          display: flex;
          align-items: center;
          gap: 12px;
        }

        .step-head .ts {
          color: var(--muted);
          font-family: var(--font-mono);
          font-size: 11px;
          letter-spacing: 0.06em;
        }

        .step-head .kind {
          flex: 1;
          color: var(--fg);
          font-size: 11.5px;
          letter-spacing: 0.02em;
        }

        .step-head .live {
          color: #fafafa;
          font-family: var(--font-mono);
          font-size: 10px;
          letter-spacing: 0.1em;
          text-transform: uppercase;
          border: 1px solid #fafafa;
          border-radius: 3px;
          padding: 1px 6px;
          display: inline-flex;
          align-items: center;
          gap: 5px;
        }

        .step-head .live::before {
          content: "";
          width: 5px;
          height: 5px;
          border-radius: 999px;
          background: #fafafa;
          animation: cov-live-pulse 1.4s ease-in-out infinite;
        }

        .running-empty {
          display: flex;
          align-items: center;
          gap: 12px;
        }

        .running-empty .empty {
          margin: 0;
        }

        .trace-live-banner {
          display: flex;
          align-items: center;
          gap: 10px;
          margin-bottom: 14px;
          padding: 8px 12px;
          border: 1px solid var(--border-soft);
          border-radius: 6px;
          background: #0a0a0a;
          color: var(--fg);
          font-family: var(--font-mono);
          font-size: 11px;
          letter-spacing: 0.08em;
          text-transform: uppercase;
        }

        .pulse {
          display: inline-block;
          width: 8px;
          height: 8px;
          border-radius: 999px;
          background: #fafafa;
          box-shadow: 0 0 0 0 rgba(250, 250, 250, 0.55);
          animation: cov-live-pulse 1.6s ease-in-out infinite;
        }

        .trace-live .trace-step:last-child .rail .dot {
          background: #fafafa;
          box-shadow: 0 0 0 0 rgba(250, 250, 250, 0.5);
          animation: cov-live-pulse 1.6s ease-in-out infinite;
        }

        @keyframes cov-live-pulse {
          0% {
            box-shadow: 0 0 0 0 rgba(250, 250, 250, 0.55);
            opacity: 1;
          }
          70% {
            box-shadow: 0 0 0 9px rgba(250, 250, 250, 0);
            opacity: 0.55;
          }
          100% {
            box-shadow: 0 0 0 0 rgba(250, 250, 250, 0);
            opacity: 1;
          }
        }

        @media (prefers-reduced-motion: reduce) {
          .pulse,
          .step-head .live::before,
          .trace-live .trace-step:last-child .rail .dot {
            animation: none;
          }
        }

        .headline {
          margin: 10px 0 4px;
          color: var(--fg);
          font-size: 13.5px;
          font-weight: 500;
        }

        .summary {
          margin: 0;
          color: var(--dim);
          font-size: 13px;
          line-height: 1.55;
        }
      `}</style>
    </>
  );
}
