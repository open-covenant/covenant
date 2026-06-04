// Env-only config (no config.json), matching the fairscale-bridge convention.
// The connector holds the LOCAL daemon bearer token and dials OUT to Hatcher,
// so the daemon never listens on a public interface (see docs/hatcher-connector-design.md §2).

export interface Config {
  // Health/metrics server (local only — Render health check, not the Hatcher channel).
  port: number;
  // Local covenantd, loopback by default. Never a public URL.
  daemonUrl: string;
  daemonToken: string;
  // True when the daemon token is a scoped connector peer (COVENANT_CONNECTOR_TOKEN)
  // rather than the operator token. Only a scoped peer makes per-dispatch grants
  // actually constrain — the operator may hold broad standing capabilities.
  usingConnectorPeer: boolean;
  daemonTimeoutMs: number;
  daemonRetries: number;
  // Outbound Hatcher mesh channel. Stubbed transport in v0 (see transport.ts).
  hatcherUrl: string;
  hatcherToken: string;
  // Single-use pairing code minted by the Hatcher UI during `covenant hatcher pair`.
  pairingCode?: string;
  // Optional path to a covenant.hatcher-agent.v0 manifest whose capabilities the
  // connector mints per dispatch. Absent ⇒ baseline (memory.write) grants only.
  manifestPath?: string;
  // Admission limits applied before any dispatch reaches the daemon.
  maxConcurrentDispatch: number;
  defaultDeadlineMs: number;
}

function required(name: string): string {
  const v = process.env[name];
  if (!v || !v.trim()) throw new Error(`missing required env ${name}`);
  return v.trim();
}

function optional(name: string): string | undefined {
  const v = process.env[name];
  return v && v.trim() ? v.trim() : undefined;
}

function int(name: string, fallback: number, { min = 1 }: { min?: number } = {}): number {
  const raw = process.env[name];
  if (raw === undefined || raw === '') return fallback;
  const n = Number(raw);
  if (!Number.isInteger(n) || n < min) throw new Error(`${name} must be an integer >= ${min}`);
  return n;
}

export function loadConfig(): Config {
  const hatcherToken = required('HATCHER_CONNECTOR_TOKEN');
  if (hatcherToken.length < 24) throw new Error('HATCHER_CONNECTOR_TOKEN must be at least 24 chars');

  return {
    port: int('PORT', int('HATCHER_CONNECTOR_PORT', 8790)),
    daemonUrl: (process.env.COVENANT_DAEMON_URL ?? 'http://127.0.0.1:8421').replace(/\/+$/, ''),
    // Prefer a scoped connector peer; fall back to the operator token for bootstrap.
    daemonToken: optional('COVENANT_CONNECTOR_TOKEN') ?? required('COVENANT_OPERATOR_TOKEN'),
    usingConnectorPeer: !!optional('COVENANT_CONNECTOR_TOKEN'),
    daemonTimeoutMs: int('COVENANT_DAEMON_TIMEOUT_MS', 15_000),
    daemonRetries: int('COVENANT_DAEMON_RETRIES', 1, { min: 0 }),
    hatcherUrl: (process.env.HATCHER_MESH_URL ?? 'wss://mesh.hatcher.local/connector').replace(/\/+$/, ''),
    hatcherToken,
    pairingCode: optional('HATCHER_PAIRING_CODE'),
    manifestPath: optional('HATCHER_MANIFEST'),
    maxConcurrentDispatch: int('HATCHER_MAX_CONCURRENT_DISPATCH', 4),
    defaultDeadlineMs: int('HATCHER_DEFAULT_DEADLINE_MS', 1_800_000),
  };
}
