"use client";

import Link from "next/link";
import { useCallback, useMemo, useState } from "react";
import { api } from "@/lib/api";
import { useDeveloperMode } from "@/lib/developerMode";
import { formatDateTime, formatTimestamp, shortHash, shortPubkey } from "@/lib/format";
import { KIND_PILL_LABELS, eventLabel } from "@/lib/labels";
import { usePoll } from "@/lib/usePoll";
import { PageHeader } from "../components/PageHeader";

async function loadAudit() {
  return api.recentAudit(100);
}

export default function ActivityLogPage() {
  const { data, error, lastSyncMs } = usePoll(loadAudit, 3000);
  const [devMode] = useDeveloperMode();
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
  const selectedLabel = selectedEvent ? eventLabel(selectedEvent) : null;

  return (
    <>
      <PageHeader
        eyebrow="signed activity"
        title="Activity log"
        subhead="Everything that happens on your daemon is signed and chained together so nothing can be quietly changed. One click verifies the whole log."
        syncMs={lastSyncMs}
        error={error}
        right={
          <button type="button" className="btn primary" onClick={verify} disabled={verifying}>
            {verifying ? "Verifying" : "Verify log"}
          </button>
        }
      />

      {verifyResult && (
        <article className={`verify-card ${verifyResult.valid ? "ok" : "bad"}`}>
          <div className="badge">{verifyResult.valid ? "INTACT" : "TAMPERED"}</div>
          <div>
            <p>
              {verifyResult.events} signed steps across {verifyResult.anchors} checkpoints.{" "}
              {verifyResult.valid
                ? "Nothing has been altered."
                : `${verifyResult.failures.length} integrity ${
                    verifyResult.failures.length === 1 ? "failure" : "failures"
                  } found.`}
            </p>
            <code>fingerprint · {shortHash(verifyResult.root, 16)}</code>
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
          <span className="eyebrow">show only</span>
          <div className="chips">
            <button
              type="button"
              className={!filter ? "chip active" : "chip"}
              onClick={() => setFilter("")}
            >
              everything <em>{events.length}</em>
            </button>
            {kinds.map((kind) => (
              <button
                type="button"
                key={kind}
                className={filter === kind ? "chip active" : "chip"}
                onClick={() => setFilter(kind)}
              >
                {KIND_PILL_LABELS[kind as keyof typeof KIND_PILL_LABELS] ?? kind}{" "}
                <em>{events.filter((e) => e.kind.type === kind).length}</em>
              </button>
            ))}
          </div>
        </div>
      </section>

      <section className="explorer">
        <div className="list">
          {filtered.length === 0 ? (
            <p className="empty">Nothing matches this filter yet.</p>
          ) : (
            <div className="records">
              {filtered.map((event) => {
                const label = eventLabel(event);
                const isSelected = selected === event.id;
                return (
                  <article
                    key={event.id}
                    className={`record clickable tone-${label.tone} ${isSelected ? "selected" : ""} fade-up`}
                    onClick={() => setSelected(event.id)}
                  >
                    <div className="ts">
                      {formatTimestamp(event.timestamp_ms)}
                      <em>{label.headline}</em>
                      {devMode && <code className="dev-kind">{event.kind.type}</code>}
                    </div>
                    <div className="body">
                      <strong>{event.issuer.display}</strong>
                      <p>{label.body}</p>
                    </div>
                    {label.intentId && (
                      <Link
                        href={`/intents/${label.intentId}`}
                        className="btn link"
                        onClick={(e) => e.stopPropagation()}
                      >
                        open task
                      </Link>
                    )}
                  </article>
                );
              })}
            </div>
          )}
        </div>
        <aside className="detail">
          {selectedEvent && selectedLabel ? (
            <>
              <div className="panel-head">
                <div>
                  <p className="eyebrow">details</p>
                  <h2>{selectedLabel.headline}</h2>
                </div>
                <button type="button" className="btn ghost" onClick={() => setSelected(null)}>
                  close
                </button>
              </div>
              <p className="lead">{selectedLabel.body}</p>
              <dl className="meta">
                <div>
                  <dt>when</dt>
                  <dd>{formatDateTime(selectedEvent.timestamp_ms)}</dd>
                </div>
                <div>
                  <dt>by</dt>
                  <dd>
                    {selectedEvent.issuer.display}
                    <em>{shortPubkey(selectedEvent.issuer.pubkey)}</em>
                  </dd>
                </div>
                <div>
                  <dt>kind</dt>
                  <dd>{KIND_PILL_LABELS[selectedEvent.kind.type]}</dd>
                </div>
              </dl>
              <details>
                <summary>raw json</summary>
                <pre className="result compact">{JSON.stringify(selectedEvent.kind, null, 2)}</pre>
              </details>
            </>
          ) : (
            <p className="empty">Pick an entry to see the signed details.</p>
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
          font-size: 11.5px;
          letter-spacing: 0.01em;
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

        .lead {
          margin: 0 0 18px;
          color: var(--fg);
          font-size: 13.5px;
          line-height: 1.5;
        }

        .meta {
          display: grid;
          gap: 12px;
          margin: 0 0 18px;
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

        details {
          margin-top: 12px;
        }

        details summary {
          cursor: pointer;
          color: var(--muted);
          font-family: var(--font-mono);
          font-size: 10px;
          letter-spacing: 0.24em;
          text-transform: uppercase;
          padding: 8px 0;
        }

        .record.selected {
          border-color: var(--fg);
          background: var(--panel-hover);
        }

        .dev-kind {
          margin-left: 8px;
          padding: 2px 6px;
          border: 1px solid var(--border-soft);
          border-radius: 4px;
          background: var(--bg);
          color: var(--muted);
          font-family: var(--font-mono);
          font-size: 10px;
          letter-spacing: 0.04em;
        }
      `}</style>
    </>
  );
}
