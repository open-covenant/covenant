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

export class HttpDaemonClient implements DaemonClient {
  constructor(
    private readonly baseUrl: string,
    private readonly token: string,
    private readonly timeoutMs = 10_000,
  ) {}

  private async get(path: string): Promise<unknown> {
    const ctrl = new AbortController();
    const t = setTimeout(() => ctrl.abort(), this.timeoutMs);
    try {
      const res = await fetch(`${this.baseUrl}${path}`, {
        headers: { authorization: `Bearer ${this.token}`, accept: 'application/json' },
        signal: ctrl.signal,
      });
      if (!res.ok) throw new Error(`daemon ${path} -> ${res.status}`);
      return res.json();
    } finally {
      clearTimeout(t);
    }
  }

  async recentAudit({ sinceMs, limit }: { sinceMs?: number; limit: number }): Promise<AuditEvent[]> {
    const q = new URLSearchParams({ limit: String(limit) });
    if (sinceMs !== undefined) q.set('since_ms', String(sinceMs));
    const body = (await this.get(`/audit/recent?${q}`)) as { kind?: string; events?: AuditEvent[]; message?: string };
    if (body.kind === 'error') throw new Error(`daemon audit error: ${body.message ?? 'unknown'}`);
    return body.events ?? [];
  }

  async verify(): Promise<IntegrityReport> {
    const body = (await this.get('/audit/verify')) as { kind?: string; report?: IntegrityReport };
    if (!body.report) throw new Error('daemon verify returned no report');
    return body.report;
  }

  async health(): Promise<boolean> {
    try {
      const ctrl = new AbortController();
      const t = setTimeout(() => ctrl.abort(), this.timeoutMs);
      const res = await fetch(`${this.baseUrl}/health`, { signal: ctrl.signal });
      clearTimeout(t);
      return res.ok;
    } catch {
      return false;
    }
  }
}
