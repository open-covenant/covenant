const BASE = process.env.NEXT_PUBLIC_COVENANT_HTTP || "http://127.0.0.1:8421";

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

async function call<T>(path: string, init?: RequestInit): Promise<T> {
  const r = await fetch(`${BASE}${path}`, {
    ...init,
    headers: { "Content-Type": "application/json", ...(init?.headers || {}) },
  });
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
};
