// Env-only config (no config.json), matching the fairscale-bridge convention.
// The connector holds the LOCAL daemon bearer token and dials OUT to Hatcher,
// so the daemon never listens on a public interface (see docs/hatcher-connector-design.md §2).

import { artifactOrigin } from './artifacts.js';

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
  hatcherReconnectBaseMs: number;
  hatcherReconnectMaxMs: number;
  agentId?: string;
  gpuEnabled: boolean;
  // Single-use pairing code minted by the Hatcher UI during `covenant hatcher pair`.
  pairingCode?: string;
  // Optional path to a covenant.hatcher-agent.v0 manifest whose capabilities the
  // connector mints per dispatch. Absent ⇒ baseline (memory.write) grants only.
  manifestPath?: string;
  // Admission limits applied before any dispatch reaches the daemon.
  maxConcurrentDispatch: number;
  defaultDeadlineMs: number;
  // Private coding-gateway origin whose GPU outputs /artifacts/:token/:name serves.
  artifactOrigin?: string;
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
  const n = raw === undefined || raw === '' ? fallback : Number(raw);
  if (!Number.isSafeInteger(n) || n < min) throw new Error(`${name} must be a safe integer >= ${min}`);
  return n;
}

export function loadConfig(): Config {
  const hatcherToken = required('HATCHER_CONNECTOR_TOKEN');
  if (hatcherToken.length < 24) throw new Error('HATCHER_CONNECTOR_TOKEN must be at least 24 chars');
  const gpuFlag = optional('HATCHER_GPU_ENABLED') ?? 'false';
  if (!['true', 'false', '1', '0'].includes(gpuFlag)) throw new Error('HATCHER_GPU_ENABLED must be true or false');
  const gpuEnabled = gpuFlag === 'true' || gpuFlag === '1';
  const agentId = optional('HATCHER_AGENT_ID');
  if (gpuEnabled && !agentId) throw new Error('GPU enablement requires HATCHER_AGENT_ID');
  const hatcherReconnectBaseMs = int('HATCHER_RECONNECT_BASE_MS', 30_000, { min: 30_000 });
  const hatcherReconnectMaxMs = int('HATCHER_RECONNECT_MAX_MS', 300_000, { min: hatcherReconnectBaseMs });

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
    hatcherReconnectBaseMs,
    hatcherReconnectMaxMs,
    agentId,
    gpuEnabled,
    pairingCode: optional('HATCHER_PAIRING_CODE'),
    manifestPath: optional('HATCHER_MANIFEST'),
    maxConcurrentDispatch: int('HATCHER_MAX_CONCURRENT_DISPATCH', 4),
    defaultDeadlineMs: int('HATCHER_DEFAULT_DEADLINE_MS', 1_800_000),
    artifactOrigin: artifactOrigin(process.env.HATCHER_ARTIFACT_ORIGIN),
  };
}
