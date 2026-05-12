"use client";

import Link from "next/link";
import { api } from "@/lib/api";
import {
  auditDetail,
  auditTone,
  eventIntentId,
  short,
  time,
  truncate,
  AUDIT_KIND_LABELS,
} from "@/lib/audit";
import { usePoll } from "@/lib/usePoll";
import { PageHeader } from "../components/PageHeader";

async function loadIntents() {
  return api.recentAudit(100);
}

export default function IntentsPage() {
  const { data, error, lastSyncMs } = usePoll(loadIntents, 3000);
  const events = data?.events ?? [];
  const intents = events
    .filter((e) => e.kind.type === "intent_dispatched")
    .slice()
    .reverse();

  return (
    <>
      <PageHeader
        eyebrow="dispatched intents"
        title="Intents"
        subhead="Every intent the daemon has dispatched. Click into one to see its full trace — capability checks, agent dispatch, memory write, receipt — anchored to the audit chain."
        syncMs={lastSyncMs}
        error={error}
      />

      <section className="panel flush">
        <div className="panel-head" style={{ padding: "22px 22px 0" }}>
          <div>
            <p className="eyebrow">recent</p>
            <h2>
              Last dispatches <span className="count">{intents.length}</span>
            </h2>
          </div>
        </div>

        {intents.length === 0 ? (
          <p className="empty" style={{ padding: "30px 22px" }}>
            no dispatches in this window — try {`"`}say hello{`"`} from the overview
          </p>
        ) : (
          <div className="intent-list">
            {intents.map((event) => {
              if (event.kind.type !== "intent_dispatched") return null;
              const intentId = eventIntentId(event);
              return (
                <Link
                  key={event.id}
                  href={`/intents/${event.kind.intent_id}`}
                  className={`intent-row tone-${auditTone(event)}`}
                >
                  <div className="left">
                    <span className="text-muted text-mono ts">{time(event.timestamp_ms)}</span>
                    <p className="text">{truncate(event.kind.intent_text, 180)}</p>
                  </div>
                  <div className="right">
                    <span className="agent">
                      {event.kind.matched_agent ? (
                        <>→ <strong>{event.kind.matched_agent}</strong></>
                      ) : (
                        <em>no agent matched</em>
                      )}
                    </span>
                    <span className="hash">{short(event.kind.result_hash_hex, 16)}</span>
                  </div>
                </Link>
              );
            })}
          </div>
        )}
      </section>

      <style jsx>{`
        .intent-list {
          display: grid;
        }

        .intent-row {
          display: flex;
          align-items: center;
          justify-content: space-between;
          gap: 22px;
          padding: 18px 22px;
          border-top: 1px solid var(--border-soft);
          color: var(--fg);
          text-decoration: none;
          transition: background 120ms ease, border-color 120ms ease;
        }

        .intent-row:hover {
          background: var(--panel-hover);
        }

        .intent-row .left {
          display: grid;
          gap: 4px;
          min-width: 0;
          flex: 1;
        }

        .intent-row .ts {
          font-size: 11px;
          letter-spacing: 0.18em;
          text-transform: uppercase;
        }

        .intent-row .text {
          margin: 0;
          font-size: 15px;
          color: var(--fg);
          line-height: 1.4;
        }

        .intent-row .right {
          display: grid;
          justify-items: end;
          gap: 4px;
          flex: 0 0 auto;
          font-family: var(--font-mono);
          font-size: 11px;
        }

        .intent-row .agent {
          color: var(--dim);
          letter-spacing: 0.04em;
        }

        .intent-row .agent strong {
          color: var(--fg);
          font-weight: 500;
        }

        .intent-row .agent em {
          color: var(--muted);
          font-style: normal;
        }

        .intent-row .hash {
          color: var(--muted);
          letter-spacing: 0.04em;
        }
      `}</style>
    </>
  );
}
