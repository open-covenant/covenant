// Translation layer between the daemon's native vocabulary (signed capability
// actions, audit kinds, runtime names, scope strings) and the operator-console
// surface. The rest of the app reaches into this file rather than reading the
// underlying primitive strings directly — that way the friendly labels stay
// consistent across pages and a single source of truth controls how Covenant
// presents itself to humans.
//
// Power users still see the underlying primitive via the existing "raw json"
// expand on each row, plus the Developer mode toggle in the sidebar which
// re-surfaces technical names beside the friendly labels.

import type { AgentEvent, AuditEvent, AuditKind } from "./api";
import { formatAgentId, formatHashStatus, formatPeerName, shortHash } from "./format";

// ──────────────────────────────────────────────────────────────────────────────
// Capability namespace catalog. Maps every signed action the daemon emits to a
// short title + one-line description in plain English.

export type PermissionMeta = {
  /** Short imperative phrase, e.g. "Save to memory" */
  title: string;
  /** One-line description, e.g. "Lets the agent persist things you tell it" */
  description: string;
  /** Visual tone for the permission card / pill. */
  risk: "low" | "med" | "high";
};

const CATALOG: Record<string, PermissionMeta> = {
  // intent.* — request lifecycle
  "intent.subscribe": {
    title: "Receive your tasks",
    description: "Required for any agent that handles requests you send.",
    risk: "low",
  },
  "intent.publish": {
    title: "Send tasks to other agents",
    description: "Allows the agent to delegate work to other agents on this machine.",
    risk: "med",
  },

  // memory.*
  "memory.read": {
    title: "Read memory",
    description: "Read records the agent or other agents have saved.",
    risk: "low",
  },
  "memory.write": {
    title: "Save to memory",
    description: "Persist results, notes, and embeddings across runs.",
    risk: "low",
  },
  "memory.purge": {
    title: "Delete memories",
    description: "Permanently remove saved records — destructive.",
    risk: "high",
  },
  "memory.search": {
    title: "Search memory",
    description: "Run semantic searches against saved records.",
    risk: "low",
  },

  // identity.*
  "identity.read": {
    title: "See identity info",
    description: "Read display name, public key, and registration time.",
    risk: "low",
  },
  "identity.rotate": {
    title: "Rotate identity keys",
    description: "Cycle the signing key for an agent — invalidates older signatures.",
    risk: "high",
  },

  // tool.* — common shipped tools
  "tool.web_search": {
    title: "Search the web",
    description: "Use the configured search provider for live web lookups.",
    risk: "med",
  },
  "tool.summarize": {
    title: "Summarize text",
    description: "Run the local summarizer over text the agent has access to.",
    risk: "low",
  },
  "tool.terminal": {
    title: "Run terminal commands",
    description: "Execute shell commands in the agent's sandbox.",
    risk: "high",
  },
  "tool.file_read": {
    title: "Read files",
    description: "Read files inside the agent's working directory.",
    risk: "med",
  },
  "tool.file_write": {
    title: "Write files",
    description: "Create or modify files in the agent's working directory.",
    risk: "high",
  },
  "tool.gpu_inference": {
    title: "Use GPU inference",
    description: "Run local model inference against your GPU.",
    risk: "med",
  },

  // agent.*
  "agent.spawn": {
    title: "Start other agents",
    description: "Launch additional agents on this machine.",
    risk: "high",
  },
  "agent.suspend": {
    title: "Pause other agents",
    description: "Suspend running agents to free resources.",
    risk: "med",
  },
};

const NAMESPACE_FALLBACKS: Array<{ prefix: string; meta: PermissionMeta }> = [
  {
    prefix: "tool.",
    meta: {
      title: "Use a tool",
      description: "Permission to call a specific tool exposed to the agent.",
      risk: "med",
    },
  },
  {
    prefix: "a2a.send.",
    meta: {
      title: "Send messages to another agent",
      description: "Allows agent-to-agent task delegation with a specific peer.",
      risk: "med",
    },
  },
  {
    prefix: "a2a.recv.",
    meta: {
      title: "Receive messages from another agent",
      description: "Allows accepting tasks sent by a specific peer.",
      risk: "low",
    },
  },
  {
    prefix: "a2a.respond.",
    meta: {
      title: "Respond to another agent",
      description: "Allows replying to a specific peer's task results.",
      risk: "low",
    },
  },
  {
    prefix: "memory.",
    meta: {
      title: "Touch memory",
      description: "Read or write a specific memory namespace.",
      risk: "low",
    },
  },
  {
    prefix: "intent.",
    meta: {
      title: "Handle tasks",
      description: "Receive, route, or forward requests.",
      risk: "low",
    },
  },
];

/**
 * Look up the friendly label for a signed capability action. Returns a
 * graceful fallback for unknown actions so we never render the raw slug
 * as a primary heading.
 */
export function permissionLabel(action: string): PermissionMeta {
  const exact = CATALOG[action];
  if (exact) return exact;
  for (const { prefix, meta } of NAMESPACE_FALLBACKS) {
    if (action.startsWith(prefix)) {
      const tail = action.slice(prefix.length);
      if (prefix.startsWith("a2a.")) {
        return {
          ...meta,
          title: `${meta.title.replace(/another agent$/i, "")}${formatPeerName(tail)}`.trim(),
        };
      }
      return {
        ...meta,
        title: `${meta.title} (${humanise(tail)})`,
      };
    }
  }
  return {
    title: humanise(action),
    description: "Permission to perform an action on this daemon.",
    risk: "med",
  };
}

function humanise(slug: string): string {
  // "memory.foo_bar" → "memory foo bar". Keep periods as gentle separators
  // so the chain of namespaces is still readable.
  return slug.replace(/[_.]/g, " ").replace(/\s+/g, " ").trim();
}

// ──────────────────────────────────────────────────────────────────────────────
// Audit event language. Each kind becomes a short headline + a single
// descriptive sentence. The shape is consistent so feeds can render every
// event with the same layout regardless of underlying kind.

export type Tone = "ok" | "warn" | "danger" | "neutral";

export type EventLabel = {
  /** Headline like "Task ran" or "Permission missing". */
  headline: string;
  /** Body sentence — what happened, in human terms. */
  body: string;
  /** Visual tone for the row. */
  tone: Tone;
  /** Optional intent UUID this row is associated with. */
  intentId: string | null;
};

function friendlyTool(tool: string): string {
  const m: Record<string, string> = {
    write_file: "Wrote a file",
    edit_file: "Edited a file",
    read_file: "Read a file",
    bash: "Ran a command",
    terminal: "Ran a command",
    web: "Fetched from the web",
    approval: "Awaiting approval",
  };
  return m[tool] ?? `Used ${tool}`;
}

// Pull a short identifier out of a tool-call preview so a streaming
// row's headline says "Ran `pytest -q`" instead of just "Ran a
// command". Daemon `preview` shapes vary by tool: bash sends the
// raw command line, web sends a URL, file ops send a path. Try a
// few common shapes; bail to null when nothing scannable comes back.
function summarizeToolPreview(tool: string, preview: string): string | null {
  const raw = (preview ?? "").trim();
  if (!raw) return null;
  // Lift the first non-empty line so multi-line shell heredocs and
  // pretty-printed JSON don't blow out the row.
  const firstLine = raw.split(/\r?\n/).find((l) => l.trim().length > 0) ?? raw;
  // If the preview is a JSON object the daemon serialised, try to
  // extract the obvious key for this tool -- keeps the headline
  // human-readable instead of dumping `{"command":"..."}` into it.
  if (firstLine.startsWith("{")) {
    try {
      const obj = JSON.parse(raw) as Record<string, unknown>;
      const key =
        tool === "bash" || tool === "terminal"
          ? (obj.command ?? obj.cmd ?? obj.script)
          : tool === "web"
            ? (obj.url ?? obj.href)
            : tool === "write_file" || tool === "edit_file" || tool === "read_file"
              ? (obj.path ?? obj.file ?? obj.filename)
              : null;
      if (typeof key === "string" && key.trim()) {
        return shortLabel(key.trim());
      }
    } catch {
      // not JSON; fall through to the raw first-line case
    }
  }
  return shortLabel(firstLine);
}

// Trim a freeform identifier to a single line fit for a headline:
// keep the first 56 chars, drop trailing whitespace, suffix an
// ellipsis when truncated.
function shortLabel(value: string): string {
  const flat = value.replace(/\s+/g, " ").trim();
  if (flat.length <= 56) return flat;
  return `${flat.slice(0, 56).trimEnd()}…`;
}

// Trim a filesystem-style path to just its trailing segment so the
// headline shows `index.html` instead of `./web/src/app/index.html`.
function basename(path: string): string {
  const m = /[^/\\]+$/.exec(path.replace(/[/\\]+$/, ""));
  return m ? m[0] : path;
}

export function eventLabel(event: AuditEvent): EventLabel {
  const kind = event.kind;
  switch (kind.type) {
    case "intent_dispatched":
      return {
        headline: kind.matched_agent
          ? `Task ran on ${formatAgentId(kind.matched_agent)}`
          : "No agent for this task",
        body: kind.matched_agent
          ? `“${truncate(kind.intent_text, 80)}”. ${formatHashStatus(kind.result_hash_hex, "Result")}.`
          : `“${truncate(kind.intent_text, 80)}”. No agent is set up to handle this kind of task — Covenant returned a default response.`,
        tone: kind.matched_agent ? "ok" : "warn",
        intentId: kind.intent_id,
      };

    case "hermes_tool_invoked":
      return {
        headline: friendlyTool(kind.tool),
        body: "The agent took a step.",
        tone: "neutral",
        intentId: kind.intent_id,
      };

    case "hermes_tool_completed":
      return {
        headline: `${friendlyTool(kind.tool)}${kind.error ? " — failed" : ""}`,
        body: `${kind.error ? "Failed" : "Done"} in ${kind.duration_ms} ms.`,
        tone: kind.error ? "danger" : "ok",
        intentId: kind.intent_id,
      };

    case "hermes_approval_requested":
      return {
        headline: "Awaiting approval",
        body: "The agent paused for a decision.",
        tone: "warn",
        intentId: kind.intent_id,
      };

    case "hermes_approval_resolved":
      return {
        headline: "Approval answered",
        body: `Chose “${kind.choice}”.`,
        tone: "neutral",
        intentId: kind.intent_id,
      };

    case "capability_check":
      if (kind.passed) {
        return {
          headline: "Permission verified",
          body: `${formatAgentRef(kind.agent_id)} can ${formatActionList(kind.required_actions)}.`,
          tone: "ok",
          intentId: extractIntentIdFromAgentId(kind.agent_id),
        };
      }
      return {
        headline: "Permission missing",
        body: `${formatAgentRef(kind.agent_id)} tried to ${formatActionList(
          kind.required_actions,
        )}, but you haven't granted ${formatActionList(kind.missing_actions)}.`,
        tone: "warn",
        intentId: extractIntentIdFromAgentId(kind.agent_id),
      };

    case "capability_granted":
      return {
        headline: "Permission granted",
        body:
          kind.granted_by_display === kind.subject_display
            ? `Allowed: ${permissionLabel(kind.action).title.toLowerCase()}.`
            : `${kind.granted_by_display} gave ${kind.subject_display} permission to ${permissionLabel(kind.action).title.toLowerCase()}.`,
        tone: "ok",
        intentId: null,
      };

    case "intent_ignored":
      return {
        headline: "Task ignored",
        body: `“${truncate(kind.intent_text, 80)}” was filtered out by your .covenantignore rule (${kind.matched_pattern}).`,
        tone: "warn",
        intentId: kind.intent_id,
      };

    case "budget_exhausted":
      return {
        headline: "Budget exhausted",
        body: `${kind.agent_display} ran out of credits while handling “${truncate(
          kind.intent_text,
          60,
        )}”. ${kind.tokens_remaining} of ${kind.requested} credits left. Refill in ${Math.ceil(
          kind.refill_eta_ms / 1000,
        )}s.`,
        tone: "danger",
        intentId: kind.intent_id,
      };

    case "budget_unseeded":
      return {
        headline: "Budget not configured",
        body: `${kind.agent_display} ran without a budget being set up. The task completed — configure a budget to enforce limits.`,
        tone: "warn",
        intentId: kind.intent_id,
      };

    case "operator_token_rotated":
      return {
        headline: "Sign-in key rotated",
        body: `Your sign-in key was refreshed for ${kind.peer_display}. The previous key no longer works.`,
        tone: "ok",
        intentId: null,
      };

    case "operator_token_rotation_rejected":
      return {
        headline: "Sign-in key rotation refused",
        body: `Tried to rotate the sign-in key for ${kind.peer_display}, but the daemon refused.`,
        tone: "danger",
        intentId: null,
      };

    case "operator_peers_list_rejected":
      return {
        headline: "Agent listing refused",
        body: `${kind.peer_display} (${shortHash(kind.peer_pubkey_b58, 8)}) tried to list connected agents and was refused.`,
        tone: "warn",
        intentId: null,
      };

    case "peer_revoked":
      return {
        headline: "Agent revoked",
        body: `${kind.peer_display} no longer trusted. Their sign-in key ${kind.token_prefix}… is rejected from now on.`,
        tone: "ok",
        intentId: null,
      };

    case "operator_peer_revoke_rejected":
      return {
        headline: "Revoke refused",
        body: `Tried to revoke ${kind.peer_display} (${shortHash(kind.peer_pubkey_b58, 8)}), but the daemon refused.`,
        tone: "danger",
        intentId: null,
      };

    case "authentication_failed":
      return {
        headline: "Sign-in refused",
        body: `Something tried to connect over ${kind.transport} without a valid sign-in key. Refused.`,
        tone: "danger",
        intentId: null,
      };

    case "a2a_result_rejected":
      return {
        headline: "Agent result rejected",
        body: `An agent-to-agent reply was rejected: ${kind.reason}. (Task ${shortHash(kind.task_id, 8)}.)`,
        tone: "danger",
        intentId: null,
      };

    case "a2a_sender_mismatch":
      return {
        headline: "Agent identity mismatch",
        body: `${kind.peer_display} sent a message claiming to be ${kind.claimed_sender_display}. Rejected.`,
        tone: "danger",
        intentId: null,
      };

    case "a2a_recipient_rejected":
      return {
        headline: "Message refused",
        body: `${kind.sender_display} tried to send a task to ${kind.recipient_display} without the right permission (${permissionLabel(kind.action).title.toLowerCase()}).`,
        tone: "danger",
        intentId: null,
      };

    case "a2a_auto_retry_scheduler_scan":
      return {
        headline: kind.enabled ? "Retry scan ran" : "Retry scan paused",
        body: `Considered ${kind.considered}, requeued ${kind.requeued}, skipped ${kind.skipped}${
          kind.error ? ` — last error: ${kind.error}` : ""
        }.`,
        tone: kind.error ? "warn" : "neutral",
        intentId: null,
      };

    case "capability_revoke_rejected":
      return {
        headline: "Revoke refused",
        body: `A permission revoke was refused: ${kind.reason}. (sig ${shortHash(kind.signature_b58, 8)}.)`,
        tone: "danger",
        intentId: null,
      };
  }

  // Fallback for a kind not in this build's typed union (e.g. a newer daemon
  // emits an audit kind this web build predates) — render a generic label
  // instead of returning undefined and crashing every caller.
  const t = String((event.kind as { type?: unknown }).type ?? "");
  return {
    headline: t ? t.replace(/_/g, " ") : "Activity",
    body: "",
    tone: "neutral",
    intentId: null,
  };
}

function formatActionList(actions: string[]): string {
  if (!actions.length) return "do something";
  const titles = actions.map((a) => permissionLabel(a).title.toLowerCase());
  if (titles.length === 1) return titles[0];
  if (titles.length === 2) return `${titles[0]} and ${titles[1]}`;
  return `${titles.slice(0, -1).join(", ")}, and ${titles[titles.length - 1]}`;
}

function formatAgentRef(agentRef: string): string {
  // Synthesized internal agent ids contain a colon and aren't user-visible
  // identities (e.g. `memory:write`, `memory-write:<uuid>`, `chain:receipts`).
  // Surface them as Covenant itself rather than as a phantom agent name.
  if (agentRef.includes(":")) {
    return "Covenant";
  }
  return agentRef;
}

function extractIntentIdFromAgentId(agentId: string): string | null {
  if (agentId.startsWith("memory-write:")) {
    return agentId.slice("memory-write:".length);
  }
  return null;
}

function truncate(value: string, length: number): string {
  if (value.length <= length) return value;
  return `${value.slice(0, length - 1)}…`;
}

// ──────────────────────────────────────────────────────────────────────────────
// AgentEvent (live SSE) label vocabulary. Mirrors `eventLabel` for audit
// events so the trace timeline can render an in-flight step the same way
// it later renders the persisted audit row, with no jarring transition
// when the SSE-pushed preview is replaced by the audit-chain redaction.

export function agentEventLabel(event: AgentEvent): EventLabel {
  switch (event.type) {
    case "reasoning": {
      const prefix = event.summary ? shortLabel(event.summary) : null;
      return {
        headline: prefix ? `Thinking — ${prefix}` : "Thinking",
        body: event.summary ? truncate(event.summary, 200) : "The agent is reasoning.",
        tone: "neutral",
        intentId: null,
      };
    }
    case "tool_call": {
      const summary = summarizeToolPreview(event.tool, event.preview);
      return {
        // Fold a short identifier (the actual command, URL, or path)
        // into the headline so streaming rows are scannable. The full
        // preview still lives in the row body for context.
        headline: summary ? `${friendlyTool(event.tool)} — ${summary}` : friendlyTool(event.tool),
        body: event.preview ? truncate(event.preview, 160) : "The agent took a step.",
        tone: "neutral",
        intentId: null,
      };
    }
    case "tool_result":
      return {
        headline:
          event.tool === "approval"
            ? "Approval resolved"
            : `${friendlyTool(event.tool)}${event.error ? " — failed" : ""}`,
        body:
          event.tool === "approval"
            ? "The agent received an answer and continued."
            : `${event.error ? "Failed" : "Done"} in ${event.duration_ms} ms.`,
        tone: event.error ? "danger" : "ok",
        intentId: null,
      };
    case "file_write":
      return {
        // Surface the file name in the headline so a multi-file build
        // reads top-to-bottom as a clear list of artifacts.
        headline: `Wrote ${basename(event.path)}`,
        body: `${event.path} · ${event.bytes} bytes.`,
        tone: "ok",
        intentId: null,
      };
  }
}

export const AGENT_EVENT_PILL_LABELS: Record<AgentEvent["type"], string> = {
  reasoning: "Thinking",
  tool_call: "Step",
  tool_result: "Step",
  file_write: "File",
};

// ──────────────────────────────────────────────────────────────────────────────
// Audit kind → short pill label used in feed columns.

export const KIND_PILL_LABELS: Record<AuditKind["type"], string> = {
  intent_dispatched: "Task",
  hermes_tool_invoked: "Step",
  hermes_tool_completed: "Step",
  hermes_approval_requested: "Approval",
  hermes_approval_resolved: "Approval",
  capability_check: "Permission check",
  capability_granted: "Permission granted",
  intent_ignored: "Task ignored",
  a2a_result_rejected: "Agent reply rejected",
  budget_exhausted: "Budget exhausted",
  budget_unseeded: "Budget missing",
  operator_token_rotated: "Sign-in key rotated",
  operator_token_rotation_rejected: "Rotation refused",
  operator_peers_list_rejected: "List refused",
  peer_revoked: "Agent revoked",
  operator_peer_revoke_rejected: "Revoke refused",
  authentication_failed: "Sign-in refused",
  a2a_sender_mismatch: "Agent mismatch",
  a2a_recipient_rejected: "Message refused",
  a2a_auto_retry_scheduler_scan: "Retry scan",
  capability_revoke_rejected: "Revoke refused",
};

// ──────────────────────────────────────────────────────────────────────────────
// Tone helpers — used to colour individual rows / status pills.

export function eventTone(event: AuditEvent): Tone {
  return eventLabel(event).tone;
}

export function isReviewWorthy(event: AuditEvent): boolean {
  const kind = event.kind;
  return (
    (kind.type === "capability_check" && !kind.passed) ||
    kind.type === "budget_exhausted" ||
    kind.type === "budget_unseeded" ||
    kind.type === "a2a_result_rejected" ||
    kind.type === "a2a_recipient_rejected" ||
    kind.type === "a2a_sender_mismatch" ||
    kind.type === "authentication_failed" ||
    kind.type === "operator_token_rotation_rejected" ||
    kind.type === "operator_peer_revoke_rejected" ||
    kind.type === "capability_revoke_rejected"
  );
}

// ──────────────────────────────────────────────────────────────────────────────
// Memory tier labels and runtime labels.

export const MEMORY_TIER_LABELS: Record<string, string> = {
  working: "Recent",
  episodic: "Session",
  longterm: "Long-term",
};

export function memoryTierLabel(tier: string): string {
  return MEMORY_TIER_LABELS[tier] ?? humanise(tier);
}

export const RUNTIME_LABELS: Record<string, string> = {
  python3: "Python",
  node: "Node.js",
  "rust-bin": "Native binary",
  hermes: "Hermes",
};
