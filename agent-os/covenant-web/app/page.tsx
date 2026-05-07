"use client";

import { useCallback, useEffect, useState } from "react";
import {
  api,
  type A2ATask,
  type A2ATaskResult,
  type AuditEvent,
  type ContentBlock,
  type Memory,
  type SettlementReceipt,
  type SignedCapability,
  type ToolSpec,
} from "@/lib/api";

export default function Home() {
  const [intent, setIntent] = useState("");
  const [submitting, setSubmitting] = useState(false);
  const [lastResult, setLastResult] = useState<string | null>(null);
  const [lastError, setLastError] = useState<string | null>(null);

  const [memories, setMemories] = useState<Memory[]>([]);
  const [capabilities, setCapabilities] = useState<SignedCapability[]>([]);

  const [search, setSearch] = useState("");
  const [searchHits, setSearchHits] = useState<Memory[] | null>(null);
  const [searching, setSearching] = useState(false);

  const [grantAction, setGrantAction] = useState("");

  const [audit, setAudit] = useState<AuditEvent[]>([]);
  const [receipts, setReceipts] = useState<SettlementReceipt[]>([]);
  const [a2aTasks, setA2aTasks] = useState<A2ATask[]>([]);
  const [a2aResults, setA2aResults] = useState<A2ATaskResult[]>([]);
  const [memoryTier, setMemoryTier] = useState<
    "" | "working" | "episodic" | "longterm"
  >("");

  const [tools, setTools] = useState<ToolSpec[]>([]);
  const [toolName, setToolName] = useState("");
  const [toolArgs, setToolArgs] = useState("{}");
  const [toolCalling, setToolCalling] = useState(false);
  const [toolResult, setToolResult] = useState<ContentBlock[] | null>(null);
  const [toolMissingCap, setToolMissingCap] = useState<string | null>(null);

  const refresh = useCallback(async () => {
    try {
      const [m, c, t, a, r, at, ar] = await Promise.all([
        api.recentMemory(20, memoryTier || undefined),
        api.recentCapabilities(20),
        api.listTools(),
        api.recentAudit(30),
        api.recentReceipts(20),
        api.recentA2ATasks(20),
        api.recentA2AResults(20),
      ]);
      setMemories(m.records);
      setCapabilities(c.capabilities);
      setTools(t.tools);
      setAudit(a.events);
      setReceipts(r.receipts);
      setA2aTasks(at.tasks);
      setA2aResults(ar.results);
      if (!toolName && t.tools.length > 0) setToolName(t.tools[0].name);
      setLastError(null);
    } catch (e) {
      setLastError(String(e));
    }
  }, [toolName, memoryTier]);

  useEffect(() => {
    // Initial fetch + 3s polling. The lint rule against calling
    // setState-bearing functions directly in an effect doesn't apply
    // cleanly to a poll loop; the interval is the reason this effect
    // exists.
    // eslint-disable-next-line react-hooks/set-state-in-effect
    refresh();
    const t = setInterval(refresh, 3000);
    return () => clearInterval(t);
  }, [refresh]);

  async function onSubmitIntent(e: React.FormEvent) {
    e.preventDefault();
    if (!intent) return;
    setSubmitting(true);
    setLastError(null);
    try {
      const r = await api.submitIntent(intent);
      if (r.kind === "intent_result") {
        setLastResult(r.text);
      } else {
        setLastError(r.message);
      }
      setIntent("");
      refresh();
    } catch (e) {
      setLastError(String(e));
    } finally {
      setSubmitting(false);
    }
  }

  async function onSearch(e: React.FormEvent) {
    e.preventDefault();
    if (!search) return;
    setSearching(true);
    try {
      const r = await api.searchMemory(search, 5);
      setSearchHits(r.records);
    } catch (e) {
      setLastError(String(e));
    } finally {
      setSearching(false);
    }
  }

  async function onGrant(e: React.FormEvent) {
    e.preventDefault();
    if (!grantAction) return;
    try {
      await api.grantCapability(grantAction);
      setGrantAction("");
      refresh();
    } catch (e) {
      setLastError(String(e));
    }
  }

  async function onRevoke(sig: string) {
    try {
      await api.revokeCapability(sig);
      refresh();
    } catch (e) {
      setLastError(String(e));
    }
  }

  async function onCallTool(e: React.FormEvent) {
    e.preventDefault();
    if (!toolName) return;
    let parsed: unknown;
    try {
      parsed = toolArgs.trim() ? JSON.parse(toolArgs) : {};
    } catch {
      setLastError(`tool args must be valid JSON: ${toolArgs}`);
      return;
    }
    setToolCalling(true);
    setToolResult(null);
    setToolMissingCap(null);
    try {
      const r = await api.callTool(toolName, parsed);
      if (r.kind === "tool_result") {
        setToolResult(r.content);
      } else {
        const required = `tool.call.${toolName}`;
        if (r.message.includes(required)) setToolMissingCap(required);
        setLastError(r.message);
      }
    } catch (e) {
      setLastError(String(e));
    } finally {
      setToolCalling(false);
    }
  }

  async function onGrantToolCap(action: string) {
    try {
      await api.grantCapability(action);
      setToolMissingCap(null);
      setLastError(null);
      refresh();
    } catch (e) {
      setLastError(String(e));
    }
  }

  return (
    <main className="page">
      <header>
        <h1>covenant</h1>
        <p className="dim">
          open agent-native operating layer · daemon at 127.0.0.1:8421
        </p>
      </header>

      <section>
        <h2>submit intent</h2>
        <form onSubmit={onSubmitIntent}>
          <textarea
            value={intent}
            onChange={(e) => setIntent(e.target.value)}
            placeholder='try: "find recent papers on agent memory"'
            rows={3}
          />
          <button type="submit" disabled={submitting || !intent}>
            {submitting ? "dispatching…" : "submit"}
          </button>
        </form>
        {lastResult && <pre className="result">{lastResult}</pre>}
        {lastError && <pre className="result error">{lastError}</pre>}
      </section>

      <section>
        <h2>capabilities</h2>
        <form onSubmit={onGrant}>
          <input
            value={grantAction}
            onChange={(e) => setGrantAction(e.target.value)}
            placeholder="action (e.g. tool.web_search)"
          />
          <button type="submit" disabled={!grantAction}>
            grant
          </button>
        </form>
        {capabilities.length === 0 ? (
          <p className="dim">(none granted)</p>
        ) : (
          <ul>
            {capabilities.map((c, i) => (
              <li key={i}>
                <span className="accent">{c.capability.action}</span>{" "}
                <span className="dim">→ {c.capability.subject.display}</span>
                <button
                  type="button"
                  className="link"
                  onClick={() => onRevoke(c.signature)}
                >
                  revoke
                </button>
              </li>
            ))}
          </ul>
        )}
      </section>

      <section>
        <h2>tools</h2>
        {tools.length === 0 ? (
          <p className="dim">(no tools registered)</p>
        ) : (
          <ul>
            {tools.map((t) => (
              <li key={t.name}>
                <span className="accent">{t.name}</span>{" "}
                <span className="dim">— {t.description}</span>
              </li>
            ))}
          </ul>
        )}
        {tools.length > 0 && (
          <form onSubmit={onCallTool}>
            <select
              value={toolName}
              onChange={(e) => {
                setToolName(e.target.value);
                setToolResult(null);
                setToolMissingCap(null);
              }}
            >
              {tools.map((t) => (
                <option key={t.name} value={t.name}>
                  {t.name}
                </option>
              ))}
            </select>
            <textarea
              value={toolArgs}
              onChange={(e) => setToolArgs(e.target.value)}
              placeholder='{"text": "hello"}'
              rows={3}
            />
            <button type="submit" disabled={toolCalling || !toolName}>
              {toolCalling ? "calling…" : "call tool"}
            </button>
          </form>
        )}
        {toolMissingCap && (
          <p className="result error">
            missing capability {toolMissingCap}{" "}
            <button
              type="button"
              className="link"
              onClick={() => onGrantToolCap(toolMissingCap)}
            >
              grant
            </button>
          </p>
        )}
        {toolResult &&
          toolResult.map((c, i) =>
            c.type === "text" ? (
              <pre key={i} className="result">
                {c.text}
              </pre>
            ) : (
              <pre key={i} className="result">
                {JSON.stringify(c.value, null, 2)}
              </pre>
            ),
          )}
      </section>

      <section>
        <h2>memory · search</h2>
        <form onSubmit={onSearch}>
          <input
            value={search}
            onChange={(e) => setSearch(e.target.value)}
            placeholder="semantic query"
          />
          <button type="submit" disabled={!search || searching}>
            {searching ? "searching…" : "search"}
          </button>
        </form>
        {searchHits !== null && (
          <ul>
            {searchHits.length === 0 ? (
              <li className="dim">(no matches)</li>
            ) : (
              searchHits.map((m) => (
                <li key={m.id}>
                  <span className="dim">[{m.tier}] </span>
                  {m.text.length > 200 ? `${m.text.slice(0, 200)}…` : m.text}
                </li>
              ))
            )}
          </ul>
        )}
      </section>

      <section>
        <h2>audit feed</h2>
        {audit.length === 0 ? (
          <p className="dim">(no audit events yet)</p>
        ) : (
          <ul>
            {audit
              .slice()
              .reverse()
              .map((e) => (
                <li key={e.id}>
                  <span className="dim">[{new Date(e.timestamp_ms).toLocaleTimeString()}] </span>
                  <span className="accent">{e.kind.type}</span>
                  {e.kind.type === "intent_dispatched" && (
                    <span className="dim">
                      {" "}
                      → {e.kind.matched_agent ?? "(none)"} ·{" "}
                      {e.kind.intent_text.length > 80
                        ? `${e.kind.intent_text.slice(0, 80)}…`
                        : e.kind.intent_text}
                    </span>
                  )}
                  {e.kind.type === "capability_check" && (
                    <span className="dim">
                      {" "}
                      {e.kind.agent_id} ·{" "}
                      {e.kind.passed ? "✓" : "✗"}{" "}
                      [{e.kind.required_actions.join(", ")}]
                    </span>
                  )}
                  {e.kind.type === "capability_granted" && (
                    <span className="dim"> {e.kind.action}</span>
                  )}
                  {e.kind.type === "intent_ignored" && (
                    <span className="dim">
                      {" "}
                      matched {e.kind.matched_pattern}
                    </span>
                  )}
                </li>
              ))}
          </ul>
        )}
      </section>

      <section>
        <h2>settlement receipts</h2>
        {receipts.length === 0 ? (
          <p className="dim">(no receipts yet)</p>
        ) : (
          <ul>
            {receipts.map((r) => (
              <li key={r.id}>
                <span className="dim">[{new Date(r.settled_at).toLocaleTimeString()}] </span>
                <span className="accent">{r.resource}</span>{" "}
                <span>{r.credits_consumed} credits</span>
                <span className="dim">
                  {" "}
                  · {r.onchain_sig ?? "(local-only)"}
                </span>
              </li>
            ))}
          </ul>
        )}
      </section>

      <section>
        <h2>recent memory</h2>
        <form>
          <select
            value={memoryTier}
            onChange={(e) =>
              setMemoryTier(e.target.value as typeof memoryTier)
            }
          >
            <option value="">all tiers</option>
            <option value="working">working</option>
            <option value="episodic">episodic</option>
            <option value="longterm">longterm</option>
          </select>
        </form>
        {memories.length === 0 ? (
          <p className="dim">(no records yet)</p>
        ) : (
          <ul>
            {memories.map((m) => (
              <li key={m.id}>
                <span className="dim">[{m.tier}] </span>
                {m.text.length > 200 ? `${m.text.slice(0, 200)}…` : m.text}
              </li>
            ))}
          </ul>
        )}
      </section>

      <section>
        <h2>queued a2a tasks</h2>
        {a2aTasks.length === 0 ? (
          <p className="dim">(no queued tasks)</p>
        ) : (
          <ul>
            {a2aTasks.map((t) => (
              <li key={t.id}>
                <span className="dim">{t.sender.display}</span>
                <span className="dim"> → </span>
                <span className="accent">{t.recipient.display}</span>
                <span className="dim">: </span>
                {t.intent_text.length > 160
                  ? `${t.intent_text.slice(0, 160)}…`
                  : t.intent_text}
              </li>
            ))}
          </ul>
        )}
      </section>

      <section>
        <h2>queued a2a results</h2>
        {a2aResults.length === 0 ? (
          <p className="dim">(no queued results)</p>
        ) : (
          <ul>
            {a2aResults.map((r) => {
              const summary =
                r.content
                  .map((c) =>
                    c.type === "text"
                      ? c.text
                      : `<json:${JSON.stringify(c.value).slice(0, 60)}…>`,
                  )
                  .join(" ")
                  .slice(0, 200) || "(empty)";
              return (
                <li key={r.task_id}>
                  <span className="dim">[{r.status}] </span>
                  <span className="dim">task=</span>
                  <span className="accent">{r.task_id.slice(0, 8)}…</span>
                  <span className="dim">: </span>
                  {summary}
                  {r.error_message ? (
                    <span className="dim"> — error: {r.error_message}</span>
                  ) : null}
                </li>
              );
            })}
          </ul>
        )}
      </section>

      <style jsx>{`
        .page {
          max-width: 960px;
          margin: 40px auto;
          padding: 0 20px;
        }
        header {
          margin-bottom: 32px;
        }
        h1 {
          font-size: 24px;
          margin-bottom: 4px;
        }
        h2 {
          font-size: 14px;
          text-transform: uppercase;
          letter-spacing: 0.08em;
          color: var(--dim);
          margin-bottom: 12px;
        }
        section {
          border-top: 1px solid var(--border);
          padding-top: 24px;
          margin-bottom: 24px;
        }
        form {
          display: flex;
          flex-direction: column;
          gap: 8px;
          margin-bottom: 12px;
        }
        button {
          align-self: flex-start;
          background: var(--accent);
          color: var(--bg);
          border: none;
          padding: 8px 16px;
          border-radius: 4px;
          font-weight: 600;
        }
        button.link {
          background: transparent;
          color: var(--dim);
          padding: 0 0 0 8px;
          font-weight: 400;
          text-decoration: underline;
        }
        button.link:hover {
          color: var(--error);
        }
        select {
          align-self: flex-start;
          padding: 6px 10px;
          background: var(--bg);
          color: var(--fg);
          border: 1px solid var(--border);
          border-radius: 4px;
          font: inherit;
        }
        ul {
          list-style: none;
        }
        li {
          padding: 8px 0;
          border-bottom: 1px solid var(--border);
          word-break: break-word;
        }
        .dim {
          color: var(--dim);
        }
        .accent {
          color: var(--accent);
        }
        .result {
          margin-top: 12px;
          padding: 12px;
          background: rgba(204, 120, 92, 0.08);
          border: 1px solid rgba(204, 120, 92, 0.3);
          border-radius: 4px;
          white-space: pre-wrap;
          word-break: break-word;
        }
        .error {
          background: rgba(248, 113, 113, 0.08);
          border-color: rgba(248, 113, 113, 0.3);
          color: var(--error);
        }
      `}</style>
    </main>
  );
}
