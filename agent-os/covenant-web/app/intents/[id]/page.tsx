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

export default function TaskTracePage(props: { params: Promise<{ id: string }> }) {
  const { id } = use(props.params);
  const [auditEvents, setAuditEvents] = useState<AuditEvent[] | null>(null);
  const [auditError, setAuditError] = useState<string | null>(null);
  const [auditSyncMs, setAuditSyncMs] = useState<number | null>(null);
  const { data: outcome } = usePoll(() => api.intentResult(id), 3000);
  const liveStream = useIntentEventStream(id);
  const [expanded, setExpanded] = useState<Set<string>>(new Set());
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
            ? `Building in the sandbox${matchedAgent ? ` · ${formatAgentId(matchedAgent)}` : ""}. The steps appear here when the run finishes.`
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
      ) : running ? (
        <div className="panel">
          <p className="empty">
            The build is running in the sandbox. Its steps — files written, commands
            run — appear here as a signed trail once the run finishes.
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
        <div className="trace">
          {traceItems.map((item, idx) => {
            const isLast = idx === traceItems.length - 1;
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
          color: var(--accent, #c9c9c9);
          font-family: var(--font-mono);
          font-size: 10px;
          letter-spacing: 0.1em;
          text-transform: uppercase;
          border: 1px solid var(--border-soft);
          border-radius: 3px;
          padding: 1px 6px;
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
