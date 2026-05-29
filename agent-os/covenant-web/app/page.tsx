"use client";

import Link from "next/link";
import { useCallback, useEffect, useState, type FormEvent } from "react";
import { api } from "@/lib/api";
import { formatRelative, formatTimestamp } from "@/lib/format";
import { saveReply } from "@/lib/intentReplies";
import { eventLabel, isReviewWorthy, memoryTierLabel } from "@/lib/labels";
import { usePoll } from "@/lib/usePoll";
import { PageHeader } from "./components/PageHeader";
import { Turnstile, turnstileEnabled } from "./components/Turnstile";
import { Markdown } from "./components/Markdown";

const DEMO_MODE = process.env.NEXT_PUBLIC_DEMO_MODE === "1";

// One-click coding demos shown as chips beneath the dispatch box. These
// showcase what the sandbox actually does — write and run real code — and are
// ordered quick → ambitious. The first entry's env var override lets ops swap
// the headline demo without a code change.
const DEMO_SAMPLES: { label: string; intent: string }[] = [
  {
    label: "Snake game",
    intent:
      process.env.NEXT_PUBLIC_DEMO_SAMPLE_INTENT?.trim() ||
      "Build a classic Snake game as a single self-contained index.html — HTML canvas, arrow-key controls, a score, and a game-over screen.",
  },
  {
    label: "3D Rubik's cube",
    intent:
      "Build a Next.js app with an interactive 3D Rubik's cube using three.js — drag to rotate, with scramble and solve buttons.",
  },
  {
    label: "Python: sudoku solver",
    intent:
      "Write a Python sudoku solver with a couple of example puzzles, and run it to print the solved grids.",
  },
];

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
  const [lastIntentId, setLastIntentId] = useState<string | null>(null);
  const [lastError, setLastError] = useState<string | null>(null);
  // Set while an async (coding) run is in flight: submit returned a
  // `running` intent and we poll its outcome until it lands.
  const [awaiting, setAwaiting] = useState(false);
  const [turnstileToken, setTurnstileToken] = useState<string | null>(null);
  const [verifying, setVerifying] = useState(false);
  const [verifyMsg, setVerifyMsg] = useState<string | null>(null);
  const [verifyOk, setVerifyOk] = useState<boolean>(true);

  const sendIntent = useCallback(
    async (text: string) => {
      const trimmed = text.trim();
      if (!trimmed) return;
      setDispatching(true);
      setLastError(null);
      setLastResult(null);
      setLastIntentId(null);
      try {
        const r = await api.submitIntent(trimmed, turnstileToken ?? undefined);
        if (r.kind === "intent_result") {
          setLastIntentId(r.intent_id);
          if (r.status === "running") {
            // Long coding build — the outcome arrives via polling below.
            setAwaiting(true);
          } else {
            setLastResult(r.text);
            saveReply(r.intent_id, r.text);
          }
        } else {
          setLastError(r.message);
        }
        setIntent("");
        await refresh();
      } catch (e) {
        setLastError(e instanceof Error ? e.message : String(e));
      } finally {
        setDispatching(false);
        if (turnstileEnabled) window.__covTurnstileReset?.();
      }
    },
    [refresh, turnstileToken],
  );

  const clearReply = useCallback(() => {
    setLastResult(null);
    setLastIntentId(null);
    setLastError(null);
    setAwaiting(false);
  }, []);

  // Poll a running coding build until it lands, then drop the reply inline.
  // Bounded: past the sandbox wall (10 min) + buffer the run is gone or stuck,
  // so we stop and say so rather than spin "building…" forever. A null result
  // (daemon forgot the run, e.g. after a restart) also ends after a grace.
  useEffect(() => {
    if (!awaiting || !lastIntentId) return;
    let cancelled = false;
    const startedAt = Date.now();
    const MAX_MS = 12 * 60 * 1000;
    let missing = 0;
    const tick = async () => {
      if (Date.now() - startedAt > MAX_MS) {
        setLastError("This run didn't finish in time — it may have been interrupted. Try again.");
        setAwaiting(false);
        return;
      }
      try {
        const o = await api.intentResult(lastIntentId);
        if (cancelled) return;
        if (!o) {
          // 404: unknown to the daemon. Transient at first; after a grace it's lost.
          if (++missing >= 8) {
            setLastError("This run was interrupted (the sandbox reset). Try again.");
            setAwaiting(false);
          }
          return;
        }
        missing = 0;
        if (o.status === "running") return;
        if (o.status === "error" || o.status === "ignored") {
          setLastError(o.text || "the run did not complete");
        } else {
          setLastResult(o.text);
          saveReply(lastIntentId, o.text);
        }
        setAwaiting(false);
      } catch {
        // transient (proxy/daemon blip) — keep polling
      }
    };
    tick();
    const t = setInterval(tick, 2500);
    return () => {
      cancelled = true;
      clearInterval(t);
    };
  }, [awaiting, lastIntentId]);

  const onDispatch = useCallback(
    (e: FormEvent) => {
      e.preventDefault();
      return sendIntent(intent);
    },
    [intent, sendIntent],
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
  // Collapse runs of the same activity (e.g. the page's own memory-read
  // permission checks every poll) into one row with a count, newest first,
  // so the feed shows distinct happenings instead of the same line repeated.
  const dedupedRecent = (() => {
    const out: { event: (typeof recent)[number]; count: number }[] = [];
    const idx = new Map<string, number>();
    for (const event of recent) {
      const label = eventLabel(event);
      const key = `${label.headline}|${label.body}|${event.issuer.pubkey}`;
      const at = idx.get(key);
      if (at !== undefined) {
        out[at].count++;
        continue;
      }
      idx.set(key, out.length);
      out.push({ event, count: 1 });
    }
    return out.slice(0, 8);
  })();
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
  const lastEvent =
    recent.find((e) => e.kind.type !== "authentication_failed") ?? recent[0] ?? null;
  const lastLabel = lastEvent ? eventLabel(lastEvent) : null;

  return (
    <>
      <PageHeader
        eyebrow={DEMO_MODE ? "public sandbox" : "local control plane"}
        title="Overview"
        subhead={
          DEMO_MODE
            ? "Describe an app or script and watch the agent write, run, and verify it in a live sandbox — every step signed and audited. Shared public sandbox; state is visible to everyone and resets periodically."
            : "Send tasks to your agents, manage their permissions, and check that the activity log is intact. Everything happens on this machine."
        }
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
            placeholder="Describe something to build — a game, a script, a small web app…"
            rows={2}
          />
          {turnstileEnabled && <Turnstile onToken={setTurnstileToken} />}
          <div className="actions">
            <button
              type="submit"
              className="btn primary"
              disabled={dispatching || !intent || (turnstileEnabled && !turnstileToken)}
            >
              {dispatching ? "Sending" : "Send"}
            </button>
          </div>
          {DEMO_MODE && (
            <div className="sample-chips">
              <span className="eyebrow text-muted">try</span>
              {DEMO_SAMPLES.map((s) => (
                <button
                  key={s.label}
                  type="button"
                  className="btn chip"
                  onClick={() => sendIntent(s.intent)}
                  disabled={dispatching}
                >
                  {s.label}
                </button>
              ))}
            </div>
          )}
        </form>

        {(lastResult || lastError || dispatching || awaiting) && (
          <div className={`reply ${lastError ? "error" : ""}`} aria-live="polite">
            <div className="reply-head">
              <p className="eyebrow">{lastError ? "error" : "reply"}</p>
              <div className="reply-head-actions">
                {lastIntentId && !lastError && (
                  <Link className="btn ghost small" href={`/intents/${lastIntentId}`}>
                    {awaiting ? "watch it work" : "open task"}
                  </Link>
                )}
                {(lastResult || lastError) && !dispatching && (
                  <button type="button" className="btn ghost small" onClick={clearReply}>
                    clear
                  </button>
                )}
              </div>
            </div>
            <div className="reply-body">
              {(dispatching || awaiting) && !lastResult && !lastError ? (
                <span className="reply-pending">
                  {awaiting
                    ? "writing and running code in the sandbox — this can take a minute or two…"
                    : "waiting for the agent…"}
                </span>
              ) : lastError ? (
                <pre>{lastError}</pre>
              ) : (
                <Markdown>{lastResult ?? ""}</Markdown>
              )}
            </div>
          </div>
        )}
      </section>

      {!DEMO_MODE && (
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
      )}

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
              {dedupedRecent.map(({ event, count }) => {
                const label = eventLabel(event);
                const RowInner = (
                  <>
                    <div className="ts">
                      {formatTimestamp(event.timestamp_ms)}
                      <em>{label.headline}</em>
                      {count > 1 && <span className="dupe">×{count}</span>}
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
        .sandbox-intro {
          margin: 18px 0 0;
          color: var(--dim);
          font-size: 13.5px;
          line-height: 1.55;
          max-width: 64ch;
        }

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

        .reply {
          margin-top: 16px;
          border: 1px solid var(--border);
          border-radius: 6px;
          background: #060606;
          overflow: hidden;
        }

        .reply.error {
          border-color: #5a1f1f;
        }

        .reply-head {
          display: flex;
          align-items: center;
          justify-content: space-between;
          gap: 12px;
          padding: 10px 14px;
          border-bottom: 1px solid var(--border);
          background: #0a0a0a;
        }

        .reply-head-actions {
          display: flex;
          align-items: center;
          gap: 8px;
        }

        .reply-body {
          padding: 14px 16px;
          max-height: 360px;
          overflow: auto;
        }

        .reply-body pre {
          margin: 0;
          color: var(--fg);
          font-family: var(--font-body);
          font-size: 14px;
          line-height: 1.55;
          white-space: pre-wrap;
          word-break: break-word;
        }

        .reply.error .reply-body pre {
          color: #f6c2c2;
          font-family: var(--font-mono);
          font-size: 12.5px;
        }

        .reply-pending {
          color: var(--muted);
          font-family: var(--font-mono);
          font-size: 12.5px;
        }

        .btn.small {
          padding: 4px 10px;
          font-size: 11px;
        }

        .metric-row {
          margin-bottom: 22px;
        }

        .records .dupe {
          margin-left: 8px;
          padding: 0 6px;
          border: 1px solid var(--border);
          border-radius: 999px;
          color: var(--muted);
          font-family: var(--font-mono);
          font-size: 10px;
          letter-spacing: 0.04em;
        }
      `}</style>
    </>
  );
}
