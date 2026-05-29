export interface Config {
  port: number;
  daemonUrl: string;
  daemonToken: string;
  daemonTimeoutMs: number;
  daemonRetries: number;
  apiToken: string;
  maxLimit: number;
  defaultLimit: number;
  fetchCap: number;
}

function required(name: string): string {
  const v = process.env[name];
  if (!v || !v.trim()) throw new Error(`missing required env ${name}`);
  return v.trim();
}

function int(name: string, fallback: number, { min = 1 }: { min?: number } = {}): number {
  const raw = process.env[name];
  if (raw === undefined || raw === '') return fallback;
  const n = Number(raw);
  if (!Number.isInteger(n) || n < min) throw new Error(`${name} must be an integer >= ${min}`);
  return n;
}

export function loadConfig(): Config {
  const apiToken = required('FAIRSCALE_API_TOKEN');
  if (apiToken.length < 24) throw new Error('FAIRSCALE_API_TOKEN must be at least 24 chars');

  const maxLimit = int('FAIRSCALE_BRIDGE_MAX_LIMIT', 1000);
  return {
    port: int('PORT', int('FAIRSCALE_BRIDGE_PORT', 8788)),
    daemonUrl: (process.env.COVENANT_DAEMON_URL ?? 'http://127.0.0.1:8421').replace(/\/+$/, ''),
    daemonToken: required('COVENANT_OPERATOR_TOKEN'),
    daemonTimeoutMs: int('COVENANT_DAEMON_TIMEOUT_MS', 15_000),
    daemonRetries: int('COVENANT_DAEMON_RETRIES', 1, { min: 0 }),
    apiToken,
    maxLimit,
    defaultLimit: Math.min(int('FAIRSCALE_BRIDGE_DEFAULT_LIMIT', 100), maxLimit),
    fetchCap: int('FAIRSCALE_BRIDGE_FETCH_CAP', 100_000),
  };
}
