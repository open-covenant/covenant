export interface AgentRef {
  display: string;
  pubkey: string;
}

export interface AuditEvent {
  id: string;
  timestamp_ms: number;
  issuer: AgentRef;
  kind: { type: string } & Record<string, unknown>;
}

export interface IntegrityReport {
  events: number;
  anchors: number;
  valid: boolean;
  root_hash_hex: string;
  failures: string[];
}

export interface DaemonClient {
  recentAudit(opts: { sinceMs?: number; limit: number }): Promise<AuditEvent[]>;
  verify(): Promise<IntegrityReport>;
  health(): Promise<boolean>;
}

class DaemonHttpError extends Error {
  constructor(readonly status: number, path: string) {
    super(`daemon ${path} -> ${status}`);
  }
}

const delay = (ms: number) => new Promise((r) => setTimeout(r, ms));

export class HttpDaemonClient implements DaemonClient {
  constructor(
    private readonly baseUrl: string,
    private readonly token: string,
    private readonly timeoutMs = 15_000,
    private readonly retries = 1,
  ) {}

  private async fetchOnce(path: string): Promise<unknown> {
    const ctrl = new AbortController();
    const timer = setTimeout(() => ctrl.abort(), this.timeoutMs);
    try {
      const res = await fetch(`${this.baseUrl}${path}`, {
        headers: { authorization: `Bearer ${this.token}`, accept: 'application/json' },
        signal: ctrl.signal,
      });
      if (!res.ok) throw new DaemonHttpError(res.status, path);
      return await res.json();
    } finally {
      clearTimeout(timer);
    }
  }

  private async getJson(path: string): Promise<unknown> {
    let lastErr: unknown;
    for (let attempt = 0; attempt <= this.retries; attempt++) {
      try {
        return await this.fetchOnce(path);
      } catch (err) {
        lastErr = err;
        const retryable = !(err instanceof DaemonHttpError) || err.status >= 500;
        if (!retryable || attempt === this.retries) break;
        await delay(150 * (attempt + 1));
      }
    }
    throw lastErr;
  }

  async recentAudit({ sinceMs, limit }: { sinceMs?: number; limit: number }): Promise<AuditEvent[]> {
    const q = new URLSearchParams({ limit: String(limit) });
    if (sinceMs !== undefined) q.set('since_ms', String(sinceMs));
    const body = (await this.getJson(`/audit/recent?${q}`)) as {
      kind?: string;
      events?: AuditEvent[];
      message?: string;
    };
    if (body.kind === 'error') throw new Error(`daemon audit error: ${body.message ?? 'unknown'}`);
    if (body.kind !== 'audit_events' || !Array.isArray(body.events)) {
      throw new Error(`unexpected daemon audit response: ${body.kind ?? 'no kind'}`);
    }
    return body.events;
  }

  async verify(): Promise<IntegrityReport> {
    const body = (await this.getJson('/audit/verify')) as { kind?: string; report?: IntegrityReport; message?: string };
    if (body.kind === 'error') throw new Error(`daemon verify error: ${body.message ?? 'unknown'}`);
    if (!body.report) throw new Error('daemon verify returned no report');
    return body.report;
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
}
