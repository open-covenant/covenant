"use client";

import {
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
  type FormEvent,
} from "react";
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
import {
  PEER_LOOKUP_LIMIT,
  expandA2aAction,
  formatExpandError,
  peerPrefixToLookup,
} from "@/lib/expand";

type PeerStatus = "" | "live" | "revoked";
type MemoryTier = "" | "working" | "episodic" | "longterm";
type Tone = "ok" | "warn" | "danger" | "neutral";

const NAV = [
  ["dashboard", "dashboard"],
  ["peers", "peers"],
  ["capabilities", "capabilities"],
  ["memory", "memory"],
  ["settlement", "settlement"],
  ["queues", "a2a queues"],
];

function emptyPeersMessage(prefix: string, status: PeerStatus): string {
  const half =
    status === "live" ? "live peers" : status === "revoked" ? "revoked peers" : "peers";
  if (prefix) return `(no ${half} match prefix)`;
  if (status) return `(no ${half})`;
  return "(no peers registered)";
}

function time(ms: number): string {
  return new Date(ms).toLocaleTimeString([], {
    hour: "2-digit",
    minute: "2-digit",
    second: "2-digit",
  });
}

function dateTime(ms: number): string {
  return new Date(ms).toLocaleString([], {
    month: "short",
    day: "2-digit",
    hour: "2-digit",
    minute: "2-digit",
  });
}

function short(value: string, length = 8): string {
  if (value.length <= length) return value;
  return `${value.slice(0, length)}...`;
}

function truncate(value: string, length: number): string {
  if (value.length <= length) return value;
  return `${value.slice(0, length)}...`;
}

function contentSummary(content: ContentBlock[]): string {
  const text = content
    .map((block) =>
      block.type === "text"
        ? block.text
        : `<json:${JSON.stringify(block.value).slice(0, 80)}...>`,
    )
    .join(" ")
    .trim();
  return truncate(text || "(empty)", 220);
}

function scopeSummary(scope: unknown): string {
  if (scope === null || scope === undefined) return "global";
  if (typeof scope !== "object") return truncate(String(scope), 120);
  const json = JSON.stringify(scope);
  if (!json || json === "{}") return "global";
  return truncate(json, 140);
}

function auditTone(event: AuditEvent): Tone {
  switch (event.kind.type) {
    case "capability_check":
      return event.kind.passed ? "ok" : "warn";
    case "capability_granted":
    case "operator_token_rotated":
    case "peer_revoked":
      return "ok";
    case "budget_exhausted":
    case "budget_unseeded":
    case "a2a_result_rejected":
    case "authentication_failed":
    case "a2a_sender_mismatch":
    case "a2a_recipient_rejected":
    case "capability_revoke_rejected":
    case "operator_token_rotation_rejected":
    case "operator_peers_list_rejected":
    case "operator_peer_revoke_rejected":
      return "danger";
    case "intent_ignored":
      return "warn";
    default:
      return "neutral";
  }
}

function auditDetail(event: AuditEvent): string {
  const kind = event.kind;
  switch (kind.type) {
    case "intent_dispatched":
      return `${kind.matched_agent ?? "(none)"} / ${truncate(kind.intent_text, 90)}`;
    case "capability_check":
      return `${kind.agent_id} / ${kind.passed ? "passed" : "missing"} / ${kind.required_actions.join(", ")}`;
    case "capability_granted":
      return `${kind.action} -> ${kind.subject_display}`;
    case "intent_ignored":
      return `matched ${kind.matched_pattern}`;
    case "a2a_result_rejected":
      return `task ${short(kind.task_id)} / ${kind.reason}`;
    case "budget_exhausted":
      return `${kind.agent_display} / ${kind.tokens_remaining}/${kind.requested} credits / ${truncate(kind.intent_text, 72)}`;
    case "budget_unseeded":
      return `${kind.agent_display} / ${kind.requested} credits requested / bucket unseeded`;
    case "operator_token_rotated":
      return `${kind.peer_display} / ${kind.old_token_prefix}... -> ${kind.new_token_prefix}...`;
    case "operator_token_rotation_rejected":
    case "operator_peers_list_rejected":
    case "operator_peer_revoke_rejected":
      return `${kind.peer_display} / pubkey ${short(kind.peer_pubkey_b58)} / rejected`;
    case "peer_revoked":
      return `${kind.peer_display} / token ${kind.token_prefix}...`;
    case "authentication_failed":
      return `${kind.transport} / ${kind.reason}`;
    case "a2a_sender_mismatch":
      return `${kind.peer_display} claimed ${kind.claimed_sender_display}`;
    case "a2a_recipient_rejected":
      return `${kind.sender_display} -> ${kind.recipient_display} / missing ${kind.action}`;
    case "a2a_auto_retry_scheduler_scan":
      return `${kind.enabled ? "enabled" : "disabled"} / considered ${kind.considered} / requeued ${kind.requeued} / skipped ${kind.skipped}${kind.error ? ` / ${kind.error}` : ""}`;
    case "capability_revoke_rejected":
      return `sig ${short(kind.signature_b58)} / ${kind.reason}`;
  }
}

function isReviewEvent(event: AuditEvent): boolean {
  const kind = event.kind;
  return (
    (kind.type === "capability_check" && !kind.passed) ||
    kind.type === "budget_exhausted" ||
    kind.type === "budget_unseeded" ||
    kind.type === "a2a_result_rejected" ||
    kind.type === "a2a_recipient_rejected"
  );
}

function errorText(error: unknown): string {
  const message = error instanceof Error ? error.message : String(error);
  if (message.includes("Failed to fetch")) {
    return "daemon unavailable at 127.0.0.1:8421";
  }
  return message;
}

export default function Home() {
  const intentRef = useRef<HTMLTextAreaElement | null>(null);
  const [intent, setIntent] = useState("");
  const [submitting, setSubmitting] = useState(false);
  const [lastResult, setLastResult] = useState<string | null>(null);
  const [lastError, setLastError] = useState<string | null>(null);
  const [refreshError, setRefreshError] = useState<string | null>(null);
  const [lastRefreshAt, setLastRefreshAt] = useState<number | null>(null);

  const [memories, setMemories] = useState<Memory[]>([]);
  const [capabilities, setCapabilities] = useState<SignedCapability[]>([]);

  const [search, setSearch] = useState("");
  const [searchHits, setSearchHits] = useState<Memory[] | null>(null);
  const [searching, setSearching] = useState(false);

  const [grantAction, setGrantAction] = useState("");
  const [grantInfo, setGrantInfo] = useState<string | null>(null);

  const [audit, setAudit] = useState<AuditEvent[]>([]);
  const [receipts, setReceipts] = useState<SettlementReceipt[]>([]);
  const [a2aTasks, setA2aTasks] = useState<A2ATask[]>([]);
  const [a2aResults, setA2aResults] = useState<A2ATaskResult[]>([]);
  const [debits, setDebits] = useState<BudgetDebit[]>([]);
  const [resuming, setResuming] = useState<string | null>(null);
  const [rotating, setRotating] = useState(false);
  const [rotatedToken, setRotatedToken] = useState<string | null>(null);
  const [memoryTier, setMemoryTier] = useState<MemoryTier>("");
  const [peers, setPeers] = useState<PeerSummary[]>([]);
  const [peersTruncated, setPeersTruncated] = useState(false);
  const [operatorPubkey, setOperatorPubkey] = useState<string>("");
  const [peerPrefix, setPeerPrefix] = useState("");
  const [peerStatus, setPeerStatus] = useState<PeerStatus>("");
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
        api.listPeers(20, peerPrefix || undefined, peerStatus || undefined),
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
      setPeersTruncated(p.truncated === true);
      setOperatorPubkey(p.operator_pubkey_b58);
      if (!toolName && t.tools.length > 0) setToolName(t.tools[0].name);
      setRefreshError(null);
      setLastRefreshAt(Date.now());
    } catch (e) {
      const message = errorText(e);
      setRefreshError(message);
      setLastError(message);
    }
  }, [toolName, memoryTier, peerPrefix, peerStatus]);

  useEffect(() => {
    // eslint-disable-next-line react-hooks/set-state-in-effect
    refresh();
    const t = setInterval(refresh, 3000);
    return () => clearInterval(t);
  }, [refresh]);

  useEffect(() => {
    function onKeyDown(event: KeyboardEvent) {
      if ((event.metaKey || event.ctrlKey) && event.key.toLowerCase() === "k") {
        event.preventDefault();
        intentRef.current?.focus();
      }
    }

    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, []);

  async function onSubmitIntent(e: FormEvent) {
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

  async function onSearch(e: FormEvent) {
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

  async function onGrant(e: FormEvent) {
    e.preventDefault();
    if (!grantAction) return;
    setLastError(null);
    setGrantInfo(null);

    let actionToGrant = grantAction;
    const prefix = peerPrefixToLookup(grantAction);
    if (prefix !== null) {
      let peersResp;
      try {
        peersResp = await api.listPeers(PEER_LOOKUP_LIMIT, prefix);
      } catch (err) {
        setLastError(String(err));
        return;
      }
      const result = expandA2aAction(grantAction, peersResp.peers);
      if (!result.ok) {
        setLastError(formatExpandError(result.error));
        return;
      }
      if (result.value.kind === "rewritten") {
        actionToGrant = result.value.full;
        setGrantInfo(`expanding \`${prefix}\` -> ${result.value.full}`);
      }
    }

    try {
      await api.grantCapability(actionToGrant);
      setGrantAction("");
      refresh();
    } catch (err) {
      setLastError(String(err));
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

  async function onCallTool(e: FormEvent) {
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
        `Revoke ${peerDisplay} (token ${tokenPrefix}...)? This is irreversible. ` +
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
        const countPhrase = r.outcome.truncated
          ? `at least ${r.outcome.matches.length} peers (list capped)`
          : `${r.outcome.matches.length} peers`;
        setLastError(
          `prefix ${tokenPrefix} matched ${countPhrase}; ` +
            "use a longer prefix via `covenant peers revoke <PREFIX>`.",
        );
      } else if (r.outcome.type === "not_found") {
        setLastError(`prefix ${tokenPrefix} matched no peers`);
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
        setLastError("clipboard permission denied");
      }
    }
  }

  const displayAudit = useMemo(() => audit.slice().reverse(), [audit]);
  const reviewRows = useMemo(
    () => displayAudit.filter(isReviewEvent).slice(0, 5),
    [displayAudit],
  );
  const activePeers = peers.filter((peer) => peer.revoked_at === null);
  const revokedPeers = peers.length - activePeers.length;
  const lastEvent = displayAudit[0] ?? null;
  const selectedTool = tools.find((tool) => tool.name === toolName) ?? null;
  const memoryCounts = {
    working: memories.filter((memory) => memory.tier === "working").length,
    episodic: memories.filter((memory) => memory.tier === "episodic").length,
    longterm: memories.filter((memory) => memory.tier === "longterm").length,
  };
  const statusLabel = refreshError ? "disconnected" : lastRefreshAt ? "connected" : "connecting";

  return (
    <main className="shell">
      <aside className="sidebar" aria-label="Covenant sections">
        <div className="brand">
          <div>
            <h1>Covenant</h1>
            <p>local web console</p>
          </div>
        </div>
        <div className={`status tone-${refreshError ? "danger" : "ok"}`}>
          <span />
          {statusLabel}
        </div>
        <nav>
          {NAV.map(([href, label]) => (
            <a key={href} href={`#${href}`}>
              {label}
            </a>
          ))}
        </nav>
        <dl className="side-facts">
          <div>
            <dt>Live peers</dt>
            <dd>{activePeers.length}</dd>
          </div>
          <div>
            <dt>Capabilities</dt>
            <dd>{capabilities.length}</dd>
          </div>
          <div>
            <dt>Review items</dt>
            <dd>{reviewRows.length}</dd>
          </div>
        </dl>
      </aside>

      <section className="workspace">
        <header id="dashboard" className="workspace-head">
          <div>
            <p className="eyebrow">covenant</p>
            <h2>Local web console</h2>
          </div>
          <div className="sync">
            {lastRefreshAt ? `synced ${time(lastRefreshAt)}` : "waiting for daemon"}
          </div>
        </header>

        <form className="command" onSubmit={onSubmitIntent}>
          <textarea
            ref={intentRef}
            value={intent}
            onChange={(e) => setIntent(e.target.value)}
            placeholder='submit an intent, for example: "summarize the last 10 commits"'
            rows={2}
          />
          <button type="submit" disabled={submitting || !intent}>
            {submitting ? "dispatching" : "dispatch"}
          </button>
        </form>

        {lastResult && <pre className="result">{lastResult}</pre>}
        {lastError && <pre className="result error">{lastError}</pre>}

        <section className="metrics" aria-label="System summary">
          <article>
            <span>Active peers</span>
            <strong>{activePeers.length}</strong>
            <p>{revokedPeers} revoked in current view</p>
          </article>
          <article>
            <span>Memory records</span>
            <strong>{memories.length}</strong>
            <p>
              {memoryCounts.working} working / {memoryCounts.episodic} episodic /{" "}
              {memoryCounts.longterm} longterm
            </p>
          </article>
          <article>
            <span>Pending review</span>
            <strong>{reviewRows.length}</strong>
            <p>failed checks, budget blocks, rejected A2A</p>
          </article>
          <article>
            <span>Last audit event</span>
            <strong>{lastEvent ? short(lastEvent.id, 10) : "--"}</strong>
            <p>{lastEvent ? auditDetail(lastEvent) : "no events yet"}</p>
          </article>
        </section>

        <section className="panel" aria-labelledby="review-title">
          <div className="panel-head">
            <div>
              <p className="eyebrow">Review loop</p>
              <h3 id="review-title">Operator attention</h3>
            </div>
          </div>
          {reviewRows.length === 0 ? (
            <p className="empty">(nothing pending)</p>
          ) : (
            <div className="records">
              {reviewRows.map((event) => {
                const kind = event.kind;
                return (
                  <article key={event.id} className={`record tone-${auditTone(event)}`}>
                    <div>
                      <span>{time(event.timestamp_ms)}</span>
                      <strong>{kind.type}</strong>
                      <p>{auditDetail(event)}</p>
                    </div>
                    {kind.type === "budget_exhausted" && (
                      <button
                        type="button"
                        className="secondary"
                        onClick={() => onResume(kind.intent_id)}
                        disabled={resuming === kind.intent_id}
                      >
                        {resuming === kind.intent_id ? "resuming" : "resume"}
                      </button>
                    )}
                  </article>
                );
              })}
            </div>
          )}
        </section>

        <section id="peers" className="panel" aria-labelledby="peers-title">
          <div className="panel-head">
            <div>
              <p className="eyebrow">mesh</p>
              <h3 id="peers-title">Registered peers</h3>
            </div>
            <form
              className="inline-controls"
              onSubmit={(e) => e.preventDefault()}
            >
              <input
                value={peerPrefix}
                onChange={(e) => setPeerPrefix(e.target.value)}
                placeholder="pubkey prefix"
              />
              <select
                value={peerStatus}
                onChange={(e) => setPeerStatus(e.target.value as PeerStatus)}
                aria-label="peer status filter"
              >
                <option value="">all</option>
                <option value="live">live</option>
                <option value="revoked">revoked</option>
              </select>
            </form>
          </div>
          {peers.length === 0 ? (
            <p className="empty">{emptyPeersMessage(peerPrefix, peerStatus)}</p>
          ) : (
            <div className="peer-grid">
              {peers.map((peer) => {
                const isSelf =
                  operatorPubkey !== "" && peer.agent_id.pubkey === operatorPubkey;
                return (
                  <article
                    key={`${peer.token_prefix}:${peer.registered_at}`}
                    className={`peer ${peer.revoked_at === null ? "live" : "revoked"}`}
                  >
                    <div>
                      <span className="avatar">{peer.agent_id.display.slice(0, 1).toUpperCase()}</span>
                      <div>
                        <strong>{peer.agent_id.display}</strong>
                        <p>
                          token {peer.token_prefix} / pubkey{" "}
                          {short(peer.agent_id.pubkey)}
                        </p>
                      </div>
                    </div>
                    <footer>
                      <span>
                        {peer.revoked_at === null
                          ? isSelf
                            ? "operator"
                            : "live"
                          : `revoked ${time(peer.revoked_at)}`}
                      </span>
                      {peer.revoked_at === null && !isSelf && (
                        <button
                          type="button"
                          className="danger-link"
                          onClick={() =>
                            onRevokePeer(peer.token_prefix, peer.agent_id.display)
                          }
                          disabled={revoking === peer.token_prefix}
                        >
                          {revoking === peer.token_prefix ? "revoking" : "revoke"}
                        </button>
                      )}
                    </footer>
                  </article>
                );
              })}
            </div>
          )}
          {peersTruncated && (
            <p className="empty">showing first {peers.length}; narrow with a prefix</p>
          )}
        </section>

        <section id="capabilities" className="panel" aria-labelledby="cap-title">
          <div className="panel-head">
            <div>
              <p className="eyebrow">permissions</p>
              <h3 id="cap-title">Capabilities</h3>
            </div>
            <form className="inline-controls wide" onSubmit={onGrant}>
              <input
                value={grantAction}
                onChange={(e) => setGrantAction(e.target.value)}
                placeholder="tool.web_search or a2a.send.<pubkey-prefix>"
              />
              <button type="submit" disabled={!grantAction}>
                grant
              </button>
            </form>
          </div>
          {grantInfo && <pre className="result compact">{grantInfo}</pre>}
          {capabilities.length === 0 ? (
            <p className="empty">(none granted)</p>
          ) : (
            <div className="capability-list">
              {capabilities.map((capability) => (
                <article key={capability.signature} className="capability">
                  <div>
                    <strong>{capability.capability.action}</strong>
                    <p>
                      {capability.capability.subject.display} / scope{" "}
                      {scopeSummary(capability.capability.scope)}
                    </p>
                    <span>sig {short(capability.signature, 12)}</span>
                  </div>
                  <button
                    type="button"
                    className="danger-link"
                    onClick={() => onRevoke(capability.signature)}
                  >
                    revoke
                  </button>
                </article>
              ))}
            </div>
          )}
        </section>

        <section id="memory" className="panel" aria-labelledby="memory-title">
          <div className="panel-head">
            <div>
              <p className="eyebrow">memory</p>
              <h3 id="memory-title">Tiers and search</h3>
            </div>
            <select
              value={memoryTier}
              onChange={(e) => setMemoryTier(e.target.value as MemoryTier)}
              aria-label="memory tier"
            >
              <option value="">all tiers</option>
              <option value="working">working</option>
              <option value="episodic">episodic</option>
              <option value="longterm">longterm</option>
            </select>
          </div>
          <form className="inline-controls wide" onSubmit={onSearch}>
            <input
              value={search}
              onChange={(e) => setSearch(e.target.value)}
              placeholder="semantic query"
            />
            <button type="submit" disabled={!search || searching}>
              {searching ? "searching" : "search"}
            </button>
          </form>
          {searchHits !== null && (
            <div className="records">
              {searchHits.length === 0 ? (
                <p className="empty">(no matches)</p>
              ) : (
                searchHits.map((memory) => (
                  <article key={memory.id} className="record">
                    <div>
                      <span>{memory.tier}</span>
                      <p>{truncate(memory.text, 240)}</p>
                    </div>
                  </article>
                ))
              )}
            </div>
          )}
          <div className="memory-list">
            {memories.length === 0 ? (
              <p className="empty">(no records yet)</p>
            ) : (
              memories.map((memory) => (
                <article key={memory.id}>
                  <span>{memory.tier}</span>
                  <p>{truncate(memory.text, 220)}</p>
                </article>
              ))
            )}
          </div>
        </section>

        <section id="settlement" className="panel split" aria-labelledby="settlement-title">
          <div className="panel-head span-all">
            <div>
              <p className="eyebrow">settlement</p>
              <h3 id="settlement-title">Credits and receipts</h3>
            </div>
          </div>
          <div>
            <h4>Budget debits</h4>
            {debits.length === 0 ? (
              <p className="empty">(no debits yet)</p>
            ) : (
              <div className="records">
                {debits.map((debit) => (
                  <article key={debit.paired_receipt} className="record">
                    <div>
                      <span>{time(debit.at_ms)}</span>
                      <strong>{debit.agent.display}</strong>
                      <p>
                        {debit.credits} credits / receipt{" "}
                        {short(debit.paired_receipt)}
                      </p>
                    </div>
                  </article>
                ))}
              </div>
            )}
          </div>
          <div>
            <h4>Receipts</h4>
            {receipts.length === 0 ? (
              <p className="empty">(no receipts yet)</p>
            ) : (
              <div className="records">
                {receipts.map((receipt) => (
                  <article key={receipt.id} className="record">
                    <div>
                      <span>{dateTime(receipt.settled_at)}</span>
                      <strong>{receipt.resource}</strong>
                      <p>
                        {receipt.credits_consumed} credits /{" "}
                        {receipt.onchain_sig ?? "local-only"}
                      </p>
                    </div>
                  </article>
                ))}
              </div>
            )}
          </div>
        </section>

        <section id="queues" className="panel split" aria-labelledby="queues-title">
          <div className="panel-head span-all">
            <div>
              <p className="eyebrow">coordination</p>
              <h3 id="queues-title">A2A queues</h3>
            </div>
          </div>
          <div>
            <h4>Tasks</h4>
            {a2aTasks.length === 0 ? (
              <p className="empty">(no queued tasks)</p>
            ) : (
              <div className="records">
                {a2aTasks.map((task) => (
                  <article key={task.id} className="record">
                    <div>
                      <span>{short(task.id)}</span>
                      <strong>
                        {task.sender.display}
                        {" -> "}
                        {task.recipient.display}
                      </strong>
                      <p>{truncate(task.intent_text, 160)}</p>
                    </div>
                  </article>
                ))}
              </div>
            )}
          </div>
          <div>
            <h4>Results</h4>
            {a2aResults.length === 0 ? (
              <p className="empty">(no queued results)</p>
            ) : (
              <div className="records">
                {a2aResults.map((result) => (
                  <article key={result.task_id} className={`record tone-${result.status === "ok" ? "ok" : "warn"}`}>
                    <div>
                      <span>{result.status}</span>
                      <strong>task {short(result.task_id)}</strong>
                      <p>
                        {contentSummary(result.content)}
                        {result.error_message ? ` / error: ${result.error_message}` : ""}
                      </p>
                    </div>
                  </article>
                ))}
              </div>
            )}
          </div>
        </section>
      </section>

      <aside className="inspector" aria-label="Inspector">
        <section className="inspector-panel">
          <div className="panel-head compact-head">
            <div>
              <p className="eyebrow">identity</p>
              <h3>Operator token</h3>
            </div>
            <button type="button" className="secondary" onClick={onRotate} disabled={rotating}>
              {rotating ? "rotating" : "rotate"}
            </button>
          </div>
          {rotatedToken && (
            <div className="token-box">
              <input
                readOnly
                value={rotatedToken}
                onFocus={(e) => e.currentTarget.select()}
              />
              <div>
                <button type="button" onClick={() => onCopyToken(rotatedToken)}>
                  copy
                </button>
                <button type="button" className="secondary" onClick={() => setRotatedToken(null)}>
                  dismiss
                </button>
              </div>
            </div>
          )}
        </section>

        <section className="inspector-panel">
          <div className="panel-head compact-head">
            <div>
              <p className="eyebrow">tools</p>
              <h3>Capability-gated call</h3>
            </div>
          </div>
          {tools.length === 0 ? (
            <p className="empty">(no tools registered)</p>
          ) : (
            <form className="tool-form" onSubmit={onCallTool}>
              <select
                value={toolName}
                onChange={(e) => {
                  setToolName(e.target.value);
                  setToolResult(null);
                  setToolMissingCap(null);
                }}
              >
                {tools.map((tool) => (
                  <option key={tool.name} value={tool.name}>
                    {tool.name}
                  </option>
                ))}
              </select>
              {selectedTool && <p>{selectedTool.description}</p>}
              <textarea
                value={toolArgs}
                onChange={(e) => setToolArgs(e.target.value)}
                placeholder='{"text": "hello"}'
                rows={4}
              />
              <button type="submit" disabled={toolCalling || !toolName}>
                {toolCalling ? "calling" : "call tool"}
              </button>
            </form>
          )}
          {toolMissingCap && (
            <p className="result error compact">
              missing {toolMissingCap}{" "}
              <button
                type="button"
                className="inline-link"
                onClick={() => onGrantToolCap(toolMissingCap)}
              >
                grant
              </button>
            </p>
          )}
          {toolResult &&
            toolResult.map((block, index) =>
              block.type === "text" ? (
                <pre key={index} className="result compact">
                  {block.text}
                </pre>
              ) : (
                <pre key={index} className="result compact">
                  {JSON.stringify(block.value, null, 2)}
                </pre>
              ),
            )}
        </section>

        <section className="inspector-panel">
          <div className="panel-head compact-head">
            <div>
              <p className="eyebrow">audit</p>
              <h3>Live trail</h3>
            </div>
          </div>
          {displayAudit.length === 0 ? (
            <p className="empty">(no audit events yet)</p>
          ) : (
            <div className="timeline">
              {displayAudit.slice(0, 12).map((event) => (
                <article key={event.id} className={`tone-${auditTone(event)}`}>
                  <span>{time(event.timestamp_ms)}</span>
                  <strong>{event.kind.type}</strong>
                  <p>{auditDetail(event)}</p>
                </article>
              ))}
            </div>
          )}
        </section>
      </aside>

      <style jsx>{`
        .shell {
          display: grid;
          grid-template-columns: 272px minmax(0, 1fr) 340px;
          min-height: 100vh;
          background: var(--bg);
        }

        .sidebar,
        .inspector {
          position: sticky;
          top: 0;
          align-self: start;
          max-height: 100vh;
          overflow: auto;
        }

        .sidebar {
          min-height: 100vh;
          border-right: 1px solid rgba(38, 38, 38, 0.8);
          background: var(--bg);
          padding: 40px 24px;
        }

        .brand {
          margin-bottom: 32px;
        }

        h1,
        h2,
        h3,
        h4,
        p {
          margin: 0;
        }

        h1 {
          color: var(--muted);
          font-size: 12px;
          font-weight: 500;
          letter-spacing: 0.4em;
          line-height: 1.2;
          text-transform: uppercase;
        }

        h2 {
          color: #fafafa;
          font-size: 36px;
          font-weight: 300;
          letter-spacing: -0.02em;
          line-height: 1.08;
        }

        h3 {
          color: #fafafa;
          font-size: 17px;
          font-weight: 400;
          letter-spacing: -0.01em;
          line-height: 1.25;
        }

        h4 {
          color: #e5e5e5;
          font-size: 12px;
          font-weight: 500;
          letter-spacing: 0.18em;
          margin-bottom: 10px;
          text-transform: uppercase;
        }

        .brand p,
        .empty,
        .sync,
        .tool-form p,
        .metrics p,
        .record p,
        .capability p,
        .peer p,
        .timeline p,
        .side-facts dt {
          color: var(--dim);
        }

        .status {
          display: flex;
          align-items: center;
          gap: 8px;
          margin-bottom: 32px;
          color: var(--dim);
          font-family: var(--font-mono);
          font-size: 11px;
          letter-spacing: 0.14em;
          text-transform: uppercase;
        }

        .status span {
          width: 7px;
          height: 7px;
          border-radius: 999px;
          background: currentColor;
        }

        nav {
          display: grid;
          gap: 6px;
          margin-bottom: 40px;
        }

        nav a {
          border-left: 2px solid transparent;
          color: var(--dim);
          font-size: 13px;
          padding: 1px 0 1px 12px;
          text-decoration: none;
          transition:
            border-color 150ms ease,
            color 150ms ease;
        }

        nav a:hover {
          border-color: var(--faint);
          color: var(--fg);
        }

        .side-facts {
          display: grid;
          gap: 16px;
        }

        .side-facts div {
          border-top: 1px solid rgba(38, 38, 38, 0.8);
          padding-top: 14px;
        }

        .side-facts dt {
          font-size: 11px;
          text-transform: uppercase;
          letter-spacing: 0.2em;
        }

        .side-facts dd {
          color: var(--fg);
          font-family: var(--font-mono);
          font-size: 20px;
          margin: 4px 0 0;
        }

        .workspace,
        .inspector {
          display: grid;
          gap: 16px;
          min-width: 0;
        }

        .workspace {
          padding: 64px 28px 96px;
        }

        .inspector {
          min-height: 100vh;
          border-left: 1px solid rgba(38, 38, 38, 0.8);
          padding: 40px 24px;
        }

        .workspace-head {
          display: flex;
          align-items: end;
          justify-content: space-between;
          gap: 16px;
          margin-bottom: 16px;
        }

        .eyebrow {
          color: var(--muted);
          font-family: var(--font-mono);
          font-size: 11px;
          letter-spacing: 0.3em;
          margin-bottom: 10px;
          text-transform: uppercase;
        }

        .command,
        .panel,
        .inspector-panel {
          border: 1px solid var(--border);
          border-radius: 6px;
          background: var(--panel);
        }

        .command {
          display: grid;
          grid-template-columns: minmax(0, 1fr) auto;
          gap: 12px;
          padding: 12px;
        }

        .command textarea {
          min-height: 58px;
          resize: vertical;
        }

        .metrics {
          display: grid;
          grid-template-columns: repeat(4, minmax(0, 1fr));
          gap: 12px;
        }

        .metrics article {
          min-width: 0;
          border: 1px solid var(--border);
          border-radius: 6px;
          background: var(--panel);
          padding: 18px;
        }

        .metrics span {
          color: var(--muted);
          font-family: var(--font-mono);
          font-size: 11px;
          text-transform: uppercase;
          letter-spacing: 0.18em;
        }

        .metrics strong {
          display: block;
          margin: 10px 0 6px;
          color: #fafafa;
          font-family: var(--font-mono);
          font-size: 28px;
          font-weight: 400;
          line-height: 1.1;
        }

        .panel,
        .inspector-panel {
          padding: 20px;
        }

        .panel-head {
          display: flex;
          align-items: start;
          justify-content: space-between;
          gap: 14px;
          margin-bottom: 14px;
        }

        .compact-head {
          align-items: center;
        }

        .inline-controls {
          display: flex;
          align-items: center;
          gap: 8px;
          min-width: min(360px, 100%);
        }

        .inline-controls.wide {
          flex: 1 1 420px;
        }

        .inline-controls input {
          min-width: 0;
        }

        .records,
        .capability-list,
        .memory-list,
        .timeline {
          display: grid;
          gap: 8px;
        }

        .record,
        .capability,
        .peer,
        .memory-list article,
        .timeline article {
          border: 1px solid var(--border-soft);
          border-radius: 6px;
          background: var(--panel);
        }

        .record,
        .capability {
          display: flex;
          justify-content: space-between;
          gap: 12px;
          padding: 12px;
        }

        .record span,
        .capability span,
        .memory-list span,
        .timeline span {
          color: var(--muted);
          font-family: var(--font-mono);
          font-size: 11px;
          text-transform: uppercase;
          letter-spacing: 0.18em;
        }

        .record strong,
        .capability strong,
        .timeline strong {
          display: block;
          color: var(--fg);
          font-weight: 500;
          margin: 3px 0;
          word-break: break-word;
        }

        .peer-grid {
          display: grid;
          grid-template-columns: repeat(auto-fit, minmax(240px, 1fr));
          gap: 10px;
        }

        .peer {
          display: grid;
          gap: 14px;
          padding: 12px;
        }

        .peer > div,
        .peer footer {
          display: flex;
          align-items: center;
          justify-content: space-between;
          gap: 10px;
        }

        .peer > div {
          justify-content: start;
        }

        .avatar {
          display: grid;
          place-items: center;
          flex: 0 0 auto;
          width: 34px;
          height: 34px;
          border: 1px solid var(--border);
          border-radius: 6px;
          color: var(--dim);
          font-family: var(--font-mono);
        }

        .peer strong {
          display: block;
        }

        .peer footer span {
          color: var(--fg);
          font-family: var(--font-mono);
          font-size: 12px;
          letter-spacing: 0.12em;
          text-transform: uppercase;
        }

        .peer.revoked footer span {
          color: var(--dim);
        }

        .capability p,
        .peer p,
        .record p,
        .timeline p,
        .memory-list p {
          word-break: break-word;
        }

        .memory-list article {
          padding: 11px 12px;
        }

        .split {
          display: grid;
          grid-template-columns: repeat(2, minmax(0, 1fr));
          gap: 16px;
        }

        .span-all {
          grid-column: 1 / -1;
          margin-bottom: 0;
        }

        .tool-form,
        .token-box {
          display: grid;
          gap: 10px;
        }

        .token-box div {
          display: flex;
          gap: 8px;
        }

        .timeline article {
          border-left: 2px solid currentColor;
          padding: 10px 12px;
        }

        .timeline strong {
          font-size: 13px;
        }

        .result {
          overflow: auto;
          max-height: 360px;
          padding: 12px;
          border: 1px solid var(--border);
          border-radius: 6px;
          background: var(--panel);
          color: var(--fg);
          font-family: var(--font-mono);
          white-space: pre-wrap;
          word-break: break-word;
        }

        .result.compact {
          max-height: 220px;
          margin-top: 10px;
          font-size: 12px;
        }

        .error {
          border-color: var(--faint);
          color: #fafafa;
        }

        .empty {
          padding: 8px 0;
        }

        .tone-ok {
          color: var(--fg);
        }

        .tone-warn {
          color: var(--dim);
        }

        .tone-danger {
          color: #fafafa;
        }

        .tone-neutral {
          color: var(--muted);
        }

        button,
        .secondary,
        .danger-link,
        .inline-link {
          border: 1px solid var(--border);
          border-radius: 6px;
          background: var(--panel);
          color: var(--fg);
          padding: 9px 13px;
          font-size: 11px;
          font-weight: 500;
          letter-spacing: 0.14em;
          text-transform: uppercase;
          white-space: nowrap;
        }

        button:hover {
          border-color: var(--faint);
          background: var(--panel-hover);
        }

        .secondary {
          background: transparent;
          border-color: var(--border);
          color: var(--dim);
        }

        .danger-link,
        .inline-link {
          align-self: center;
          background: transparent;
          border-color: transparent;
          color: var(--dim);
          padding: 4px 0;
        }

        .danger-link:hover,
        .inline-link:hover {
          background: transparent;
          color: var(--fg);
        }

        input,
        select,
        textarea {
          min-width: 0;
          border: 1px solid var(--border);
          border-radius: 6px;
          background: var(--bg);
          color: var(--fg);
          padding: 10px 11px;
        }

        textarea {
          font-family: var(--font-mono);
        }

        select {
          height: 40px;
        }

        input:focus,
        select:focus,
        textarea:focus {
          border-color: var(--faint);
          outline: none;
        }

        @media (max-width: 1180px) {
          .shell {
            grid-template-columns: 240px minmax(0, 1fr);
          }

          .inspector {
            position: static;
            grid-column: 2;
            max-height: none;
            min-height: 0;
          }

          .metrics {
            grid-template-columns: repeat(2, minmax(0, 1fr));
          }
        }

        @media (max-width: 900px) {
          .shell {
            grid-template-columns: 1fr;
          }

          .sidebar,
          .inspector {
            position: static;
            grid-column: auto;
            max-height: none;
            min-height: 0;
          }

          .sidebar,
          .workspace,
          .inspector {
            padding: 24px;
          }

          .sidebar,
          .inspector {
            border-right: 0;
            border-left: 0;
            border-bottom: 1px solid rgba(38, 38, 38, 0.8);
          }

          nav {
            grid-template-columns: repeat(2, minmax(0, 1fr));
          }

          .workspace-head,
          .panel-head,
          .command,
          .split,
          .inline-controls {
            grid-template-columns: 1fr;
            flex-direction: column;
            align-items: stretch;
          }

          .metrics {
            grid-template-columns: 1fr;
          }
        }
      `}</style>
    </main>
  );
}
