"use client";

import { useCallback, useEffect, useState } from "react";
import {
  api,
  setRuntimeToken,
  type A2ATask,
  type A2ATaskResult,
  type AuditEvent,
  type BudgetDebit,
  type ContentBlock,
  type Memory,
  type PeerSummary,
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
  const [debits, setDebits] = useState<BudgetDebit[]>([]);
  const [resuming, setResuming] = useState<string | null>(null);
  const [rotating, setRotating] = useState(false);
  const [rotatedToken, setRotatedToken] = useState<string | null>(null);
  const [memoryTier, setMemoryTier] = useState<
    "" | "working" | "episodic" | "longterm"
  >("");
  const [peers, setPeers] = useState<PeerSummary[]>([]);
  const [operatorPubkey, setOperatorPubkey] = useState<string>("");
  const [peerPrefix, setPeerPrefix] = useState("");
  const [revoking, setRevoking] = useState<string | null>(null);

  const [tools, setTools] = useState<ToolSpec[]>([]);
  const [toolName, setToolName] = useState("");
  const [toolArgs, setToolArgs] = useState("{}");
  const [toolCalling, setToolCalling] = useState(false);
  const [toolResult, setToolResult] = useState<ContentBlock[] | null>(null);
  const [toolMissingCap, setToolMissingCap] = useState<string | null>(null);

  const refresh = useCallback(async () => {
    try {
      const [m, c, t, a, r, at, ar, d, p] = await Promise.all([
        api.recentMemory(20, memoryTier || undefined),
        api.recentCapabilities(20),
        api.listTools(),
        api.recentAudit(30),
        api.recentReceipts(20),
        api.recentA2ATasks(20),
        api.recentA2AResults(20),
        api.recentDebits(20),
        api.listPeers(20, peerPrefix || undefined),
      ]);
      setMemories(m.records);
      setCapabilities(c.capabilities);
      setTools(t.tools);
      setAudit(a.events);
      setReceipts(r.receipts);
      setA2aTasks(at.tasks);
      setA2aResults(ar.results);
      setDebits(d.debits);
      setPeers(p.peers);
      setOperatorPubkey(p.operator_pubkey_b58);
      if (!toolName && t.tools.length > 0) setToolName(t.tools[0].name);
      setLastError(null);
    } catch (e) {
      setLastError(String(e));
    }
  }, [toolName, memoryTier, peerPrefix]);

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

  async function onResume(intent_id: string) {
    setResuming(intent_id);
    setLastError(null);
    try {
      const r = await api.resumeIntent(intent_id);
      if (r.kind === "intent_result") {
        setLastResult(r.text);
      } else {
        setLastError(r.message);
      }
      refresh();
    } catch (e) {
      setLastError(String(e));
    } finally {
      setResuming(null);
    }
  }

  async function onRotate() {
    if (
      typeof window !== "undefined" &&
      !window.confirm(
        "Rotate the operator token? The current bootstrap token will be revoked. " +
          "Other shells holding it will need to re-read peers/operator.token.",
      )
    ) {
      return;
    }
    setRotating(true);
    setLastError(null);
    try {
      const r = await api.rotateOperatorToken();
      if (r.kind === "operator_token_rotated") {
        setRuntimeToken(r.token_b58);
        setRotatedToken(r.token_b58);
        refresh();
      } else {
        setLastError(r.message);
      }
    } catch (e) {
      setLastError(String(e));
    } finally {
      setRotating(false);
    }
  }

  async function onRevokePeer(tokenPrefix: string, peerDisplay: string) {
    if (
      typeof window !== "undefined" &&
      !window.confirm(
        `Revoke ${peerDisplay} (token ${tokenPrefix}…)? This is irreversible. ` +
          "Future Authenticate frames presenting this token will be rejected.",
      )
    ) {
      return;
    }
    setRevoking(tokenPrefix);
    setLastError(null);
    try {
      const r = await api.revokePeer(tokenPrefix);
      if (r.kind === "error") {
        setLastError(r.message);
      } else if (r.outcome.type === "ambiguous") {
        setLastError(
          `prefix ${tokenPrefix} matched ${r.outcome.matches.length} peers; ` +
            "use a longer prefix via `covenant peers revoke <PREFIX>`.",
        );
      } else if (r.outcome.type === "not_found") {
        setLastError(
          `prefix ${tokenPrefix} matched no peers (concurrent revoke?)`,
        );
      }
      refresh();
    } catch (e) {
      setLastError(String(e));
    } finally {
      setRevoking(null);
    }
  }

  async function onCopyToken(token: string) {
    if (typeof navigator !== "undefined" && navigator.clipboard) {
      try {
        await navigator.clipboard.writeText(token);
      } catch {
        /* permissions denied — operator can copy from the input manually */
      }
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
        <h2>operator token</h2>
        <p className="dim">
          rotate the bootstrap token at $COVENANT_HOME/peers/operator.token.
          the new token is written to disk and stored in this tab so polling
          continues without a dev-server restart.
        </p>
        <button type="button" onClick={onRotate} disabled={rotating}>
          {rotating ? "rotating…" : "rotate operator token"}
        </button>
        {rotatedToken && (
          <div className="result">
            <p>
              new token (also at $COVENANT_HOME/peers/operator.token, mode 0600):
            </p>
            <input
              readOnly
              value={rotatedToken}
              onFocus={(e) => e.currentTarget.select()}
            />
            <button type="button" onClick={() => onCopyToken(rotatedToken)}>
              copy
            </button>
            <button type="button" onClick={() => setRotatedToken(null)}>
              dismiss
            </button>
            <p className="dim">
              paste into .env.development.local as NEXT_PUBLIC_COVENANT_TOKEN
              for future builds. existing shells need to re-read the file.
            </p>
          </div>
        )}
      </section>

      <section>
        <h2>registered peers</h2>
        <p className="dim">
          newest-first; revoked entries kept so a post-incident pubkey from
          the audit feed resolves to a live or tombstoned row in one read.
          paste a pubkey b58 prefix below to filter — same encoding as the
          audit row&apos;s peer_pubkey_b58.
        </p>
        <form onSubmit={(e) => e.preventDefault()}>
          <input
            value={peerPrefix}
            onChange={(e) => setPeerPrefix(e.target.value)}
            placeholder="pubkey b58 prefix (paste from audit row)"
          />
        </form>
        {peers.length === 0 ? (
          <p className="dim">
            {peerPrefix ? "(no peers match prefix)" : "(no peers registered)"}
          </p>
        ) : (
          <ul>
            {peers.map((p) => {
              const isSelf =
                operatorPubkey !== "" && p.agent_id.pubkey === operatorPubkey;
              return (
                <li key={p.token_prefix + ":" + p.registered_at}>
                  <span className="dim">
                    [{new Date(p.registered_at).toLocaleTimeString()}]{" "}
                  </span>
                  <span className={p.revoked_at !== null ? "dim" : "accent"}>
                    {p.agent_id.display}
                  </span>
                  {isSelf && <span className="dim"> (self)</span>}{" "}
                  <span className="dim">
                    · token {p.token_prefix}… · pubkey{" "}
                    {p.agent_id.pubkey.slice(0, 8)}…
                    {p.revoked_at !== null && (
                      <>
                        {" "}
                        · revoked {new Date(p.revoked_at).toLocaleTimeString()}
                      </>
                    )}
                  </span>
                  {p.revoked_at === null && !isSelf && (
                    <button
                      type="button"
                      className="link"
                      onClick={() =>
                        onRevokePeer(p.token_prefix, p.agent_id.display)
                      }
                      disabled={revoking === p.token_prefix}
                    >
                      {revoking === p.token_prefix ? "revoking…" : "revoke"}
                    </button>
                  )}
                  {p.revoked_at === null && isSelf && (
                    <span className="dim">
                      {" "}
                      · use rotate token above to replace
                    </span>
                  )}
                </li>
              );
            })}
          </ul>
        )}
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
                  {e.kind.type === "a2a_result_rejected" && (
                    <span className="dim">
                      {" "}
                      task={e.kind.task_id.slice(0, 8)}… · {e.kind.reason}
                    </span>
                  )}
                  {e.kind.type === "budget_exhausted" &&
                    (() => {
                      const k = e.kind;
                      return (
                        <>
                          <span className="dim">
                            {" "}
                            {k.agent_display} · {k.tokens_remaining}/
                            {k.requested} credits ·{" "}
                            {k.intent_text.length > 60
                              ? `${k.intent_text.slice(0, 60)}…`
                              : k.intent_text}
                          </span>
                          <button
                            type="button"
                            className="link"
                            onClick={() => onResume(k.intent_id)}
                            disabled={resuming === k.intent_id}
                          >
                            {resuming === k.intent_id ? "resuming…" : "resume"}
                          </button>
                        </>
                      );
                    })()}
                  {e.kind.type === "budget_unseeded" && (
                    <span className="dim">
                      {" "}
                      {e.kind.agent_display} · {e.kind.requested} credits
                      requested · bucket unseeded (operator misconfig)
                    </span>
                  )}
                  {e.kind.type === "operator_token_rotated" && (
                    <span className="dim">
                      {" "}
                      {e.kind.peer_display} · {e.kind.old_token_prefix}… →{" "}
                      {e.kind.new_token_prefix}…
                    </span>
                  )}
                  {e.kind.type === "operator_token_rotation_rejected" && (
                    <span className="dim">
                      {" "}
                      {e.kind.peer_display} · pubkey{" "}
                      {e.kind.peer_pubkey_b58.slice(0, 8)}… · rejected
                      (non-operator)
                    </span>
                  )}
                  {e.kind.type === "operator_peers_list_rejected" && (
                    <span className="dim">
                      {" "}
                      {e.kind.peer_display} · pubkey{" "}
                      {e.kind.peer_pubkey_b58.slice(0, 8)}… · rejected
                      (non-operator)
                    </span>
                  )}
                  {e.kind.type === "peer_revoked" && (
                    <span className="dim">
                      {" "}
                      {e.kind.peer_display} · pubkey{" "}
                      {e.kind.peer_pubkey_b58.slice(0, 8)}… · token{" "}
                      {e.kind.token_prefix}…
                    </span>
                  )}
                  {e.kind.type === "operator_peer_revoke_rejected" && (
                    <span className="dim">
                      {" "}
                      {e.kind.peer_display} · pubkey{" "}
                      {e.kind.peer_pubkey_b58.slice(0, 8)}… · rejected
                      (non-operator)
                    </span>
                  )}
                  {e.kind.type === "authentication_failed" && (
                    <span className="dim">
                      {" "}
                      {e.kind.transport} · {e.kind.reason}
                    </span>
                  )}
                  {e.kind.type === "a2a_sender_mismatch" && (
                    <span className="dim">
                      {" "}
                      peer={e.kind.peer_display} · claimed=
                      {e.kind.claimed_sender_display}
                    </span>
                  )}
                  {e.kind.type === "a2a_recipient_rejected" && (
                    <span className="dim">
                      {" "}
                      {e.kind.sender_display} → {e.kind.recipient_display} ·{" "}
                      missing {e.kind.action}
                    </span>
                  )}
                  {e.kind.type === "capability_revoke_rejected" && (
                    <span className="dim">
                      {" "}
                      sig={e.kind.signature_b58.slice(0, 8)}… · {e.kind.reason}
                    </span>
                  )}
                </li>
              ))}
          </ul>
        )}
      </section>

      <section>
        <h2>budget debits</h2>
        {debits.length === 0 ? (
          <p className="dim">(no debits yet)</p>
        ) : (
          <ul>
            {debits.map((d) => (
              <li key={d.paired_receipt}>
                <span className="dim">
                  [{new Date(d.at_ms).toLocaleTimeString()}]{" "}
                </span>
                <span className="accent">{d.agent.display}</span>{" "}
                <span>{d.credits} credits</span>{" "}
                <span className="dim">
                  · receipt={d.paired_receipt.slice(0, 8)}…
                </span>
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
