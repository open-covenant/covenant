"use client";

import Link from "next/link";
import { use, useState } from "react";
import { api } from "@/lib/api";
import { eventsForIntent } from "@/lib/audit";
import { formatTimestamp, shortHash } from "@/lib/format";
import { KIND_PILL_LABELS, eventLabel } from "@/lib/labels";
import { usePoll } from "@/lib/usePoll";
import { PageHeader } from "../../components/PageHeader";

async function loadIntent() {
  return api.recentAudit(200);
}

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

export default function TaskTracePage(props: { params: Promise<{ id: string }> }) {
  const { id } = use(props.params);
  const { data, error, lastSyncMs } = usePoll(loadIntent, 3000);
  const [expanded, setExpanded] = useState<Set<string>>(new Set());

  const events = data?.events ?? [];
  const trace = eventsForIntent(events, id);
  const dispatched = trace.find((e) => e.kind.type === "intent_dispatched");
  const dispatchKind = dispatched?.kind.type === "intent_dispatched" ? dispatched.kind : null;
  const totalDurationMs =
    trace.length > 1 ? trace[trace.length - 1].timestamp_ms - trace[0].timestamp_ms : null;
  const isLoading = data === null && error === null;

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
        title={dispatchKind ? `“${dispatchKind.intent_text}”` : "Task"}
        subhead={
          dispatchKind
            ? dispatchKind.matched_agent
              ? `Ran by ${dispatchKind.matched_agent}. The result is signed (${shortHash(dispatchKind.result_hash_hex, 10)}) so it can’t be quietly changed.`
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
          <strong>{trace.length}</strong>
        </article>
        <article className="meta-cell">
          <p className="eyebrow">took</p>
          <strong>{totalDurationMs == null ? "—" : `${totalDurationMs} ms`}</strong>
        </article>
        <article className="meta-cell">
          <p className="eyebrow">ran by</p>
          <strong>{dispatchKind?.matched_agent ?? "—"}</strong>
        </article>
        <article className="meta-cell">
          <p className="eyebrow">status</p>
          <strong>{statusWord(dispatchKind?.status)}</strong>
        </article>
      </section>

      {isLoading ? (
        <div className="panel">
          <p className="empty">Loading the task&apos;s steps…</p>
        </div>
      ) : trace.length === 0 ? (
        <div className="panel">
          <p className="empty">
            Can&apos;t find this task in the local log. It may be older than what&apos;s cached on this machine.
          </p>
        </div>
      ) : (
        <div className="trace">
          {trace.map((event, idx) => {
            const isLast = idx === trace.length - 1;
            const isExpanded = expanded.has(event.id);
            const label = eventLabel(event);
            return (
              <article key={event.id} className={`trace-step tone-${label.tone}`}>
                <div className="rail">
                  <span className="dot" />
                  {!isLast && <span className="line" />}
                </div>
                <div className="step-card">
                  <div className="step-head">
                    <span className="ts">{formatTimestamp(event.timestamp_ms, { withSeconds: true })}</span>
                    <span className="kind">{KIND_PILL_LABELS[event.kind.type]}</span>
                    <button
                      type="button"
                      className="btn link"
                      onClick={() => toggle(event.id)}
                    >
                      {isExpanded ? "hide raw" : "raw json"}
                    </button>
                  </div>
                  <p className="headline">{label.headline}</p>
                  <p className="summary">{label.body}</p>
                  {isExpanded && (
                    <pre className="result compact">{JSON.stringify(event, null, 2)}</pre>
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
