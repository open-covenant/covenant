export interface Config {
  port: number;
  daemonUrl: string;
  daemonToken: string;
  apiToken: string;
  maxLimit: number;
  defaultLimit: number;
}

function required(name: string): string {
  const v = process.env[name];
  if (!v || !v.trim()) throw new Error(`missing required env ${name}`);
  return v.trim();
}

export function loadConfig(): Config {
  const apiToken = required('FAIRSCALE_API_TOKEN');
  if (apiToken.length < 24) throw new Error('FAIRSCALE_API_TOKEN must be at least 24 chars');
  const maxLimit = Number(process.env.FAIRSCALE_BRIDGE_MAX_LIMIT ?? 1000);
  return {
    port: Number(process.env.PORT ?? process.env.FAIRSCALE_BRIDGE_PORT ?? 8788),
    daemonUrl: (process.env.COVENANT_DAEMON_URL ?? 'http://127.0.0.1:8421').replace(/\/+$/, ''),
    daemonToken: required('COVENANT_OPERATOR_TOKEN'),
    apiToken,
    maxLimit,
    defaultLimit: Math.min(Number(process.env.FAIRSCALE_BRIDGE_DEFAULT_LIMIT ?? 100), maxLimit),
  };
}
