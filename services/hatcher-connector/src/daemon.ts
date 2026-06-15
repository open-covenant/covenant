// Daemon-facing half of the connector. Every shape here is verified against
// covenantd source in this worktree (file:line refs in comments). This is the
// part that is REAL today — the Hatcher transport (transport.ts) is the stubbed
// inferred half.

export interface AgentRef {
  display: string;
  pubkey: string;
}

// Live trace projection, covenant-runtime/src/lib.rs:154 (RuntimeTrace -> AgentEvent).
// Approvals surface as a `tool_result` with tool="approval" (verified); `reasoning`
// is reserved. The trailing open member is forward-compat: unknown chunk types are
// relayed verbatim rather than dropped, so a daemon that adds an event kind doesn't
// require a connector release.
export type AgentEvent =
  | { type: 'tool_call'; run_id: string; tool: string; preview: string }
  | { type: 'tool_result'; run_id: string; tool: string; duration_ms: number; error: boolean }
  | { type: 'file_write'; run_id: string; path: string; bytes: number }
  | { type: 'reasoning'; run_id: string; preview?: string }
  | { type: string; run_id?: string; [k: string]: unknown };

// POST /intent response, covenant-ipc Response::IntentResult.
export interface IntentResult {
  intent_id: string;
  status: string; // "ok" | "running" | "error" | "ignored"
  text: string;
  sources: string[];
}

// /audit/verify report.
export interface IntegrityReport {
  events: number;
  anchors: number;
  valid: boolean;
  root_hash_hex: string;
  failures: string[];
}

// Capability grant request, http.rs:559 (GrantBody) — NOTE: no `subject` field;
// the grant binds to the caller (the connector). See design §5.
export interface GrantRequest {
  action: string;
  scope?: unknown;
  expires_at?: number;
}

// A2A mailbox shapes, verified against covenant-a2a/src/lib.rs:109,387 and the
// covenantd handlers. AgentId serializes as a {display, pubkey} object on the wire
// (pubkey base58) — recipient is NOT a bare string.
export type A2ATaskStatus = 'ok' | 'error' | 'partial';
export interface A2AContent {
  type: string;
  [k: string]: unknown;
}
export interface A2ATask {
  id: string;
  sender: AgentRef;
  recipient: AgentRef;
  intent_text: string;
  task_kind?: string;
  parent?: string;
  deadline_ms?: number;
  // duplicate_safety is the A2ADuplicateSafety enum (snake_case); omit the whole
  // idempotency block unless a known variant is configured.
  idempotency?: { duplicate_safety: string; key: string };
}
export interface A2ATaskResult {
  task_id: string;
  status: A2ATaskStatus;
  content: A2AContent[];
  error_message?: string;
}

// Kept separate from DaemonClient so the intent-path fakes don't have to implement
// it; HttpDaemonClient implements both.
export interface A2ADaemon {
  sendA2ATask(task: A2ATask): Promise<unknown>;
  recvA2ATask(): Promise<A2ATask | null>;
  postA2AResult(result: A2ATaskResult): Promise<unknown>;
  recvA2AResult(): Promise<A2ATaskResult | null>;
}

// POST /identity/sign response (pairing leg-3). Verified live: ed25519 signature over
// "covenant.identity.attest.v1\n" || message, gated by the identity.attest capability.
export interface Attestation {
  signature_b58: string;
  pubkey_b58: string;
  ts: number;
}

class DaemonHttpError extends Error {
  constructor(readonly status: number, path: string) {
    super(`daemon ${path} -> ${status}`);
  }
}

// The daemon could not be reached at all (connection refused, DNS, timeout/abort) —
// distinct from the daemon answering with an error. Lets the connector report
// `daemon_unreachable` instead of misclassifying a down daemon as a policy denial.
export class DaemonUnreachableError extends Error {
  constructor(detail: string) {
    super(`daemon unreachable (${detail})`);
    this.name = 'DaemonUnreachableError';
  }
}

const delay = (ms: number) => new Promise((r) => setTimeout(r, ms));

export interface DaemonClient {
  health(): Promise<boolean>;
  submitIntent(text: string): Promise<IntentResult>;
  intentResult(intentId: string): Promise<unknown>;
  streamEvents(intentId: string, onEvent: (e: AgentEvent) => void, signal: AbortSignal): Promise<void>;
  verify(): Promise<IntegrityReport>;
  grant(req: GrantRequest): Promise<void>;
}

export class HttpDaemonClient implements DaemonClient, A2ADaemon {
  constructor(
    private readonly baseUrl: string,
    private readonly token: string,
    private readonly timeoutMs = 15_000,
    private readonly retries = 1,
  ) {}

  private headers(extra: Record<string, string> = {}): Record<string, string> {
    return { authorization: `Bearer ${this.token}`, ...extra };
  }

  private async fetchJson(path: string, init?: RequestInit): Promise<unknown> {
    let lastErr: unknown;
    for (let attempt = 0; attempt <= this.retries; attempt++) {
      const ctrl = new AbortController();
      const timer = setTimeout(() => ctrl.abort(), this.timeoutMs);
      try {
        const res = await fetch(`${this.baseUrl}${path}`, {
          ...init,
          headers: this.headers({ accept: 'application/json', ...(init?.headers as Record<string, string>) }),
          signal: ctrl.signal,
        });
        if (!res.ok) throw new DaemonHttpError(res.status, path);
        return await res.json();
      } catch (err) {
        lastErr = err;
        const retryable = !(err instanceof DaemonHttpError) || err.status >= 500;
        if (!retryable || attempt === this.retries) break;
        await delay(150 * (attempt + 1));
      } finally {
        clearTimeout(timer);
      }
    }
    if (lastErr instanceof DaemonHttpError) throw lastErr;
    throw new DaemonUnreachableError(`${path}: ${String(lastErr)}`);
  }

  async health(): Promise<boolean> {
    const ctrl = new AbortController();
    const timer = setTimeout(() => ctrl.abort(), this.timeoutMs);
    try {
      const res = await fetch(`${this.baseUrl}/health`, { signal: ctrl.signal });
      return res.ok;
    } catch {
      return false;
    } finally {
      clearTimeout(timer);
    }
  }

  // POST /intent { text }  -> IntentResult  (http.rs:279, :303)
  async submitIntent(text: string): Promise<IntentResult> {
    const body = (await this.fetchJson('/intent', {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify({ text }),
    })) as Partial<IntentResult> & { kind?: string; message?: string };
    if (body.kind === 'error') throw new Error(`daemon intent error: ${body.message ?? 'unknown'}`);
    if (typeof body.intent_id !== 'string' || typeof body.status !== 'string') {
      throw new Error('unexpected /intent response shape');
    }
    return {
      intent_id: body.intent_id,
      status: body.status,
      text: body.text ?? '',
      sources: body.sources ?? [],
    };
  }

  // GET /intents/:id/result -> Response::IntentResult { intent_id, status, text, sources, settlement? }
  // (same shape as POST /intent; verified covenant-ipc Response::IntentResult)
  async intentResult(intentId: string): Promise<unknown> {
    return this.fetchJson(`/intents/${intentId}/result`);
  }

  // GET /intents/:id/events (SSE) -> AgentEvent stream (http.rs:1007, sse.rs:51)
  async streamEvents(intentId: string, onEvent: (e: AgentEvent) => void, signal: AbortSignal): Promise<void> {
    const res = await fetch(`${this.baseUrl}/intents/${intentId}/events`, {
      headers: this.headers({ accept: 'text/event-stream' }),
      signal,
    });
    if (!res.ok || !res.body) throw new DaemonHttpError(res.status, `/intents/${intentId}/events`);
    const reader = res.body.getReader();
    const decoder = new TextDecoder();
    let buf = '';
    for (;;) {
      const { done, value } = await reader.read();
      if (done) break;
      buf += decoder.decode(value, { stream: true });
      // SSE frames are separated by a blank line. A `:` line is a keepalive comment.
      let sep: number;
      while ((sep = buf.indexOf('\n\n')) !== -1) {
        const frame = buf.slice(0, sep);
        buf = buf.slice(sep + 2);
        const dataLine = frame.split('\n').find((l) => l.startsWith('data:'));
        if (!dataLine) continue;
        const json = dataLine.slice('data:'.length).trim();
        if (!json) continue;
        try {
          // /intents/:id/events emits the raw AgentEvent per frame
          // (`data: {"type":...}`, covenantd http.rs agent_event_sse_frame), not
          // the v2 stream_chunk envelope. Parse the event directly.
          const ev = JSON.parse(json) as AgentEvent;
          if (ev && typeof (ev as { type?: unknown }).type === 'string') {
            onEvent(ev);
          }
        } catch {
          // tolerate partial frames / keepalive comments
        }
      }
    }
  }

  // GET /audit/verify -> IntegrityReport (operator-global root; see design §2.5 honesty note)
  async verify(): Promise<IntegrityReport> {
    const body = (await this.fetchJson('/audit/verify')) as { kind?: string; report?: IntegrityReport; message?: string };
    if (body.kind === 'error') throw new Error(`daemon verify error: ${body.message ?? 'unknown'}`);
    if (!body.report) throw new Error('daemon verify returned no report');
    return body.report;
  }

  // POST /capabilities/grant { action, scope?, expires_at? } (http.rs:559, :568)
  async grant(req: GrantRequest): Promise<void> {
    const body = (await this.fetchJson('/capabilities/grant', {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify(req),
    })) as { kind?: string; message?: string };
    if (body.kind === 'error') throw new Error(`daemon grant error: ${body.message ?? 'unknown'}`);
  }

  // POST /a2a/tasks { A2ATask } -> Response (http.rs:729)
  async sendA2ATask(task: A2ATask): Promise<unknown> {
    const body = (await this.fetchJson('/a2a/tasks', {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify(task),
    })) as { kind?: string; message?: string };
    if (body.kind === 'error') throw new Error(`daemon a2a send error: ${body.message ?? 'unknown'}`);
    return body;
  }

  // GET /a2a/tasks/next -> { kind:"a2_a_task_opt", task: A2ATask | null } (http.rs:147)
  async recvA2ATask(): Promise<A2ATask | null> {
    const body = (await this.fetchJson('/a2a/tasks/next')) as { task?: A2ATask | null };
    return body.task ?? null;
  }

  // POST /a2a/results { A2ATaskResult } -> Response (http.rs:746)
  async postA2AResult(result: A2ATaskResult): Promise<unknown> {
    const body = (await this.fetchJson('/a2a/results', {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify(result),
    })) as { kind?: string; message?: string };
    if (body.kind === 'error') throw new Error(`daemon a2a result error: ${body.message ?? 'unknown'}`);
    return body;
  }

  // GET /a2a/results/next -> { kind:"a2_a_result_opt", result: A2ATaskResult | null } (http.rs:150)
  async recvA2AResult(): Promise<A2ATaskResult | null> {
    const body = (await this.fetchJson('/a2a/results/next')) as { result?: A2ATaskResult | null };
    return body.result ?? null;
  }

  // POST /identity/sign { message_b58, ts } -> IdentityAttestation (pairing leg-3)
  async signAttestation(messageB58: string, ts: number): Promise<Attestation> {
    const body = (await this.fetchJson('/identity/sign', {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify({ message_b58: messageB58, ts }),
    })) as { kind?: string; message?: string; signature_b58?: string; pubkey_b58?: string; ts?: number };
    if (body.kind === 'error') throw new Error(`daemon attestation error: ${body.message ?? 'unknown'}`);
    if (!body.signature_b58 || !body.pubkey_b58) throw new Error('unexpected /identity/sign response');
    return { signature_b58: body.signature_b58, pubkey_b58: body.pubkey_b58, ts: body.ts ?? ts };
  }
}
