const BASE = process.env.NEXT_PUBLIC_COVENANT_HTTP || "http://127.0.0.1:8421";
const BUILD_TOKEN = process.env.NEXT_PUBLIC_COVENANT_TOKEN || "";

// Sprint 60: the bootstrap token is build-time-baked, but `RotateOperatorToken`
// can mint a fresh one at runtime. Persist the rotated token in localStorage
// so the live tab keeps working without a dev-server restart; fall back to
// the build-time value when localStorage is empty (or unavailable, e.g. SSR).
const TOKEN_KEY = "covnt_token";

function readToken(): string {
  if (typeof window !== "undefined") {
    try {
      const v = window.localStorage.getItem(TOKEN_KEY);
      if (v) return v;
    } catch {
      /* localStorage may be disabled (private mode, etc.) — fall through */
    }
  }
  return BUILD_TOKEN;
}

export function setRuntimeToken(token: string): void {
  if (typeof window === "undefined") return;
  try {
    window.localStorage.setItem(TOKEN_KEY, token);
  } catch {
    /* swallow — the rotation still succeeded server-side */
  }
}

export type Memory = {
  id: string;
  tier: string;
  text: string;
  created_at: number;
};

export type SignedCapability = {
  capability: {
    subject: { display: string; pubkey: string };
    action: string;
    granted_by: { display: string; pubkey: string };
    expires_at: number | null;
    scope: unknown;
  };
  signature: string;
};

export type IntentResult =
  | {
      kind: "intent_result";
      intent_id: string;
      status: string;
      text: string;
      sources: string[];
      settlement: unknown;
    }
  | { kind: "error"; message: string };

export type ToolSpec = {
  name: string;
  description: string;
  inputSchema: unknown;
};

export type ContentBlock =
  | { type: "text"; text: string }
  | { type: "json"; value: unknown };

export type ToolCallResponse =
  | {
      kind: "tool_result";
      content: ContentBlock[];
      is_error: boolean;
    }
  | { kind: "error"; message: string };

export type AuditKind =
  | {
      type: "intent_dispatched";
      intent_id: string;
      intent_text: string;
      matched_agent: string | null;
      result_hash_hex: string;
      status: string;
    }
  | {
      type: "capability_check";
      agent_id: string;
      required_actions: string[];
      missing_actions: string[];
      passed: boolean;
    }
  | {
      type: "capability_granted";
      subject_display: string;
      action: string;
      granted_by_display: string;
      signature_b58: string;
    }
  | {
      type: "intent_ignored";
      intent_id: string;
      intent_text: string;
      matched_pattern: string;
    }
  | {
      type: "a2a_result_rejected";
      task_id: string;
      reason: string;
    }
  | {
      type: "budget_exhausted";
      agent_display: string;
      intent_id: string;
      intent_text: string;
      requested: number;
      tokens_remaining: number;
      refill_eta_ms: number;
    }
  | {
      type: "budget_unseeded";
      agent_display: string;
      intent_id: string;
      requested: number;
    }
  | {
      type: "operator_token_rotated";
      peer_display: string;
      old_token_prefix: string;
      new_token_prefix: string;
    }
  | {
      type: "operator_token_rotation_rejected";
      peer_display: string;
      peer_pubkey_b58: string;
    }
  | {
      type: "operator_peers_list_rejected";
      peer_display: string;
      peer_pubkey_b58: string;
    }
  | {
      type: "peer_revoked";
      peer_display: string;
      peer_pubkey_b58: string;
      token_prefix: string;
    }
  | {
      type: "operator_peer_revoke_rejected";
      peer_display: string;
      peer_pubkey_b58: string;
    }
  | {
      type: "authentication_failed";
      transport: string;
      reason: string;
    }
  | {
      type: "a2a_sender_mismatch";
      peer_display: string;
      claimed_sender_display: string;
    }
  | {
      type: "a2a_recipient_rejected";
      sender_display: string;
      recipient_display: string;
      action: string;
    }
  | {
      type: "capability_revoke_rejected";
      signature_b58: string;
      reason: string;
    };

export type AuditEvent = {
  id: string;
  timestamp_ms: number;
  issuer: { display: string; pubkey: number[] };
  kind: AuditKind;
};

export type SettlementReceipt = {
  id: string;
  payer: { display: string; pubkey: string };
  resource: "compute" | "memory" | "tool" | "message" | "registration";
  credits_consumed: number;
  settled_at: number;
  onchain_sig: string | null;
};

export type AgentId = { display: string; pubkey: string };

export type A2ATask = {
  id: string;
  sender: AgentId;
  recipient: AgentId;
  intent_text: string;
  parent: string | null;
  deadline_ms: number | null;
};

export type A2ATaskResult = {
  task_id: string;
  status: "ok" | "error" | "partial";
  content: ContentBlock[];
  error_message: string | null;
};

export type BudgetDebit = {
  agent: AgentId;
  credits: number;
  paired_receipt: string;
  at_ms: number;
};

// Sprint 62 wire shape: full token bytes never appear; `token_prefix` is
// 6 chars matching the audit log's `*_token_prefix` redaction. `revoked_at`
// is `Some(ts)` for tombstoned entries — kept on purpose so post-incident
// triage can answer "is this audit-flagged peer already revoked?" in one
// look. Newest-first.
export type PeerSummary = {
  agent_id: AgentId;
  token_prefix: string;
  registered_at: number;
  revoked_at: number | null;
};

// Sprint 65 wire shape. Internally tagged on `type`; newtype variants
// flatten `PeerSummary`'s fields into the wrapper. Token bytes never
// carried — only `PeerSummary` (excludes `PeerToken` by Sprint 62
// invariant).
export type RevokeOutcome =
  | ({ type: "revoked" } & PeerSummary)
  | ({ type: "already_revoked" } & PeerSummary)
  | { type: "not_found" }
  | { type: "ambiguous"; matches: PeerSummary[] };

async function call<T>(path: string, init?: RequestInit): Promise<T> {
  const headers: Record<string, string> = {
    "Content-Type": "application/json",
    ...((init?.headers as Record<string, string>) || {}),
  };
  const token = readToken();
  if (token) {
    headers["Authorization"] = `Bearer ${token}`;
  }
  const r = await fetch(`${BASE}${path}`, { ...init, headers });
  if (!r.ok) {
    const body = await r.text().catch(() => "");
    throw new Error(`${path} → ${r.status}: ${body}`);
  }
  return r.json();
}

export const api = {
  health: () => call<{ status: string }>("/health"),

  submitIntent: (text: string) =>
    call<IntentResult>("/intent", {
      method: "POST",
      body: JSON.stringify({ text }),
    }),

  recentMemory: (limit = 20, tier?: "working" | "episodic" | "longterm") =>
    call<{ kind: "memories"; records: Memory[] }>(
      `/memory/recent?limit=${limit}${tier ? `&tier=${tier}` : ""}`,
    ),

  searchMemory: (q: string, limit = 10) =>
    call<{ kind: "memories"; records: Memory[] }>(
      `/memory/search?q=${encodeURIComponent(q)}&limit=${limit}`,
    ),

  recentCapabilities: (limit = 20) =>
    call<{ kind: "capabilities"; capabilities: SignedCapability[] }>(
      `/capabilities/recent?limit=${limit}`,
    ),

  grantCapability: (action: string) =>
    call<{
      kind: "capability_granted";
      signature_b58: string;
      subject_display: string;
      action: string;
    }>("/capabilities/grant", {
      method: "POST",
      body: JSON.stringify({ action }),
    }),

  revokeCapability: (signature_b58: string) =>
    call<{ kind: "capability_revoked"; signature_b58: string; removed: boolean }>(
      "/capabilities/revoke",
      {
        method: "POST",
        body: JSON.stringify({ signature_b58 }),
      },
    ),

  listTools: () =>
    call<{ kind: "tool_list"; tools: ToolSpec[] }>("/tools"),

  callTool: (name: string, args: unknown) =>
    call<ToolCallResponse>("/tools/call", {
      method: "POST",
      body: JSON.stringify({ name, arguments: args }),
    }),

  recentAudit: (limit = 30) =>
    call<{ kind: "audit_events"; events: AuditEvent[] }>(
      `/audit/recent?limit=${limit}`,
    ),

  recentReceipts: (limit = 20) =>
    call<{ kind: "receipts"; receipts: SettlementReceipt[] }>(
      `/receipts/recent?limit=${limit}`,
    ),

  recentA2ATasks: (limit = 20) =>
    call<{ kind: "a2a_tasks"; tasks: A2ATask[] }>(
      `/a2a/tasks/recent?limit=${limit}`,
    ),

  recentA2AResults: (limit = 20) =>
    call<{ kind: "a2a_results"; results: A2ATaskResult[] }>(
      `/a2a/results/recent?limit=${limit}`,
    ),

  recentDebits: (limit = 20) =>
    call<{ kind: "debits"; debits: BudgetDebit[] }>(
      `/budget/debits?limit=${limit}`,
    ),

  resumeIntent: (intent_id: string) =>
    call<IntentResult>("/intents/resume", {
      method: "POST",
      body: JSON.stringify({ intent_id }),
    }),

  rotateOperatorToken: () =>
    call<
      | { kind: "operator_token_rotated"; token_b58: string }
      | { kind: "error"; message: string }
    >("/peers/rotate", {
      method: "POST",
      body: JSON.stringify({}),
    }),

  listPeers: (limit = 20, prefix?: string) => {
    const q = new URLSearchParams({ limit: String(limit) });
    if (prefix) q.set("prefix", prefix);
    return call<{
      kind: "peer_list";
      peers: PeerSummary[];
      operator_pubkey_b58: string;
    }>(`/peers/list?${q.toString()}`);
  },

  revokePeer: (token_prefix: string) =>
    call<
      | { kind: "peer_revoked"; outcome: RevokeOutcome }
      | { kind: "error"; message: string }
    >("/peers/revoke", {
      method: "POST",
      body: JSON.stringify({ token_prefix }),
    }),
};
