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

// Sample prompts shown as chips beneath the dispatch box in demo mode.
// The first entry is the default "Try a sample task" target; the env var
// override is kept for ops who want to swap it without a code change.
const DEMO_SAMPLES: { label: string; intent: string }[] = [
  {
    label: "Say hi",
    intent:
      process.env.NEXT_PUBLIC_DEMO_SAMPLE_INTENT?.trim() ||
      "Say hi and tell me what you can do.",
  },
  {
    label: "What is Covenant?",
    intent: "Hi — explain Covenant in one short paragraph.",
  },
  {
    label: "What just happened?",
    intent:
      "Hi — walk me through what just happened when I sent this: capability check, dispatch, signing, audit log.",
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
  useEffect(() => {
    if (!awaiting || !lastIntentId) return;
    let cancelled = false;
    const tick = async () => {
      try {
        const o = await api.intentResult(lastIntentId);
        if (cancelled || !o || o.status === "running") return;
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
            ? "Send tasks to your agents, manage their permissions, and check that the activity log is intact. This is a shared public sandbox — state is visible to everyone and resets periodically."
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

      {DEMO_MODE && (
        <p className="sandbox-intro">
          This is a live Covenant daemon. Dispatch a task below and watch it get signed,
          audited, and stored — everything runs in a safe sandbox.
        </p>
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
                    ? "building in the sandbox — this can take a minute…"
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
      `}</style>
    </>
  );
}
