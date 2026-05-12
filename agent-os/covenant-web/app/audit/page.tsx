"use client";

import Link from "next/link";
import { useCallback, useMemo, useState } from "react";
import { api } from "@/lib/api";
import {
  AUDIT_KIND_LABELS,
  auditDetail,
  auditTone,
  eventIntentId,
  short,
  time,
} from "@/lib/audit";
import { usePoll } from "@/lib/usePoll";
import { PageHeader } from "../components/PageHeader";

async function loadAudit() {
  return api.recentAudit(100);
}

export default function AuditPage() {
  const { data, error, lastSyncMs } = usePoll(loadAudit, 3000);
  const [filter, setFilter] = useState<string>("");
  const [verifying, setVerifying] = useState(false);
  const [verifyResult, setVerifyResult] = useState<{
    valid: boolean;
    root: string;
    events: number;
    anchors: number;
    failures: string[];
  } | null>(null);
  const [selected, setSelected] = useState<string | null>(null);

  const verify = useCallback(async () => {
    setVerifying(true);
    try {
      const r = await api.verifyAudit();
      setVerifyResult({
        valid: r.report.valid,
        root: r.report.root_hash_hex,
        events: r.report.events,
        anchors: r.report.anchors,
        failures: r.report.failures,
      });
    } finally {
      setVerifying(false);
    }
  }, []);

  const events = data?.events ?? [];
  const reversed = events.slice().reverse();
  const filtered = filter ? reversed.filter((e) => e.kind.type === filter) : reversed;
  const kinds = useMemo(() => {
    const set = new Set(events.map((e) => e.kind.type));
    return Array.from(set);
  }, [events]);
  const selectedEvent = selected ? events.find((e) => e.id === selected) : null;

  return (
    <>
      <PageHeader
        eyebrow="signed · hash-chained"
        title="Audit"
        subhead="Append-only JSONL with a SHA-256 chain sidecar. Every state transition lands here, and the chain root is verifiable in one click."
        syncMs={lastSyncMs}
        error={error}
        right={
          <button type="button" className="btn primary" onClick={verify} disabled={verifying}>
            {verifying ? "verifying" : "verify chain"}
          </button>
        }
      />

      {verifyResult && (
        <article className={`verify-card ${verifyResult.valid ? "ok" : "bad"}`}>
          <div className="badge">{verifyResult.valid ? "VALID" : "INVALID"}</div>
          <div>
            <p>
              {verifyResult.events} events across {verifyResult.anchors} anchors.{" "}
              {verifyResult.valid
                ? "Every retained row hashes correctly into its successor."
                : `${verifyResult.failures.length} integrity failures detected.`}
            </p>
            <code>root · {verifyResult.root}</code>
            {verifyResult.failures.length > 0 && (
              <ul>
                {verifyResult.failures.slice(0, 5).map((failure, idx) => (
                  <li key={idx}>{failure}</li>
                ))}
              </ul>
            )}
          </div>
        </article>
      )}

      <section className="filter-row">
        <div className="filter-group">
          <span className="eyebrow">filter by kind</span>
          <div className="chips">
            <button
              type="button"
              className={!filter ? "chip active" : "chip"}
              onClick={() => setFilter("")}
            >
              all <em>{events.length}</em>
            </button>
            {kinds.map((kind) => (
              <button
                type="button"
                key={kind}
                className={filter === kind ? "chip active" : "chip"}
                onClick={() => setFilter(kind)}
              >
                {AUDIT_KIND_LABELS[kind as keyof typeof AUDIT_KIND_LABELS] ?? kind}{" "}
                <em>{events.filter((e) => e.kind.type === kind).length}</em>
              </button>
            ))}
          </div>
        </div>
      </section>

      <section className="explorer">
        <div className="list">
          {filtered.length === 0 ? (
            <p className="empty">no events match this filter</p>
          ) : (
            <div className="records">
              {filtered.map((event) => {
                const intentId = eventIntentId(event);
                const isSelected = selected === event.id;
                return (
                  <article
                    key={event.id}
                    className={`record clickable tone-${auditTone(event)} ${isSelected ? "selected" : ""} fade-up`}
                    onClick={() => setSelected(event.id)}
                  >
                    <div className="ts">
                      {time(event.timestamp_ms)}
                      <em>{AUDIT_KIND_LABELS[event.kind.type]}</em>
                    </div>
                    <div className="body">
                      <strong>{event.issuer.display}</strong>
                      <p>{auditDetail(event)}</p>
                    </div>
                    {intentId && (
                      <Link
                        href={`/intents/${intentId}`}
                        className="btn link"
                        onClick={(e) => e.stopPropagation()}
                      >
                        trace
                      </Link>
                    )}
                  </article>
                );
              })}
            </div>
          )}
        </div>
        <aside className="detail">
          {selectedEvent ? (
            <>
              <div className="panel-head">
                <div>
                  <p className="eyebrow">event detail</p>
                  <h2>{AUDIT_KIND_LABELS[selectedEvent.kind.type]}</h2>
                </div>
                <button type="button" className="btn ghost" onClick={() => setSelected(null)}>
                  close
                </button>
              </div>
              <dl className="meta">
                <div>
                  <dt>id</dt>
                  <dd className="text-mono">{selectedEvent.id}</dd>
                </div>
                <div>
                  <dt>issuer</dt>
                  <dd>
                    {selectedEvent.issuer.display}
                    <em>{short(selectedEvent.issuer.pubkey, 22)}</em>
                  </dd>
                </div>
                <div>
                  <dt>timestamp</dt>
                  <dd>{new Date(selectedEvent.timestamp_ms).toISOString()}</dd>
                </div>
              </dl>
              <p className="eyebrow">raw kind</p>
              <pre className="result compact">{JSON.stringify(selectedEvent.kind, null, 2)}</pre>
            </>
          ) : (
            <p className="empty">select an event to see signed envelope + raw JSON</p>
          )}
        </aside>
      </section>

      <style jsx>{`
        .verify-card {
          display: flex;
          gap: 18px;
          align-items: flex-start;
          padding: 18px 22px;
          margin-bottom: 22px;
          border: 1px solid var(--border);
          border-radius: 8px;
          background: var(--panel);
        }

        .verify-card.bad {
          border-color: var(--faint);
        }

        .verify-card .badge {
          padding: 4px 12px;
          border: 1px solid var(--border);
          border-radius: 999px;
          color: #fafafa;
          font-family: var(--font-mono);
          font-size: 10px;
          letter-spacing: 0.32em;
        }

        .verify-card.ok .badge {
          background: rgba(255, 255, 255, 0.04);
        }

        .verify-card p {
          margin: 0 0 8px;
          color: var(--dim);
          font-size: 13px;
        }

        .verify-card code {
          display: block;
          padding: 8px 12px;
          background: var(--bg);
          border: 1px solid var(--border-soft);
          border-radius: 4px;
          color: var(--fg);
          font-family: var(--font-mono);
          font-size: 11.5px;
          word-break: break-all;
        }

        .verify-card ul {
          margin: 8px 0 0;
          padding: 0 0 0 18px;
          color: var(--dim);
          font-size: 12px;
        }

        .filter-row {
          margin-bottom: 18px;
        }

        .filter-group .eyebrow {
          display: block;
          margin-bottom: 10px;
        }

        .chips {
          display: flex;
          flex-wrap: wrap;
          gap: 6px;
        }

        .chip {
          display: inline-flex;
          align-items: center;
          gap: 8px;
          padding: 6px 12px;
          border: 1px solid var(--border);
          border-radius: 999px;
          background: var(--panel);
          color: var(--dim);
          font-size: 11px;
          letter-spacing: 0.04em;
          text-transform: lowercase;
          transition: border-color 120ms ease, color 120ms ease;
        }

        .chip em {
          color: var(--muted);
          font-family: var(--font-mono);
          font-size: 10px;
          font-style: normal;
        }

        .chip:hover {
          border-color: var(--faint);
          color: var(--fg);
        }

        .chip.active {
          border-color: var(--fg);
          color: var(--fg);
        }

        .explorer {
          display: grid;
          grid-template-columns: 1fr 380px;
          gap: 16px;
          align-items: start;
        }

        @media (max-width: 1100px) {
          .explorer {
            grid-template-columns: 1fr;
          }
        }

        .detail {
          padding: 22px;
          border: 1px solid var(--border);
          border-radius: 8px;
          background: var(--panel);
          position: sticky;
          top: 24px;
        }

        .meta {
          display: grid;
          gap: 12px;
          margin: 18px 0;
        }

        .meta div {
          display: grid;
          gap: 4px;
        }

        .meta dt {
          color: var(--muted);
          font-family: var(--font-mono);
          font-size: 10px;
          letter-spacing: 0.24em;
          text-transform: uppercase;
        }

        .meta dd {
          margin: 0;
          color: var(--fg);
          font-size: 13px;
          word-break: break-all;
        }

        .meta dd em {
          display: block;
          margin-top: 2px;
          color: var(--muted);
          font-family: var(--font-mono);
          font-size: 11px;
          font-style: normal;
        }

        .record.selected {
          border-color: var(--fg);
          background: var(--panel-hover);
        }
      `}</style>
    </>
  );
}
