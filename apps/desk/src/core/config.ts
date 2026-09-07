/**
 * Configuration: defaults, validation, and where the desk keeps its files.
 *
 * Home directory resolution, in order:
 *   1. `DESK_HOME`, used by tests and by anyone running more than one desk.
 *   2. `XDG_CONFIG_HOME/covenant-desk`.
 *   3. `~/.config/covenant-desk`.
 */

import { randomBytes } from 'node:crypto';
import { chmodSync, mkdirSync, readFileSync, writeFileSync } from 'node:fs';
import { homedir } from 'node:os';
import path from 'node:path';
import { z } from 'zod';
import { ConfigError } from './errors.js';

/** Robinhood Chain. */
export const CHAIN_ID = 4663;
/** Public Robinhood Chain RPC. */
export const DEFAULT_RPC_URL = 'https://rpc.mainnet.chain.robinhood.com';
/** Loopback port for the local HTTP API and the UI. */
export const DEFAULT_PORT = 46631;

const BoundsSchema = z.object({
  /** Largest single order, USD. */
  maxOrderNotionalUsd: z.number().positive().default(250),
  /** Largest total filled in a rolling 24 hours, USD. */
  maxDailyNotionalUsd: z.number().positive().default(1000),
  /** Widest gap accepted between quote and fill, basis points. */
  maxSlippageBps: z.number().int().min(0).max(10_000).default(100),
  /** Highest stock-leg premium a buy may pay, basis points. */
  maxBuyPremiumBps: z.number().int().min(0).max(100_000).default(500),
});

const HedgeSchema = z.object({
  /** Rebalance when the short drifts this far from target, basis points. */
  driftBps: z.number().int().min(1).max(10_000).default(500),
  /** Rebalance when funding runs against the position by more than this over eight hours, basis points. */
  maxFundingBps8h: z.number().min(0).default(50),
  /** How often the rebalancer compares target against current, seconds. */
  intervalSec: z.number().int().min(5).default(60),
  /** Largest notional the hedge client may send in one order, USD. */
  maxNotionalUsd: z.number().positive().default(1000),
  /** Lighter Robinhood Chain REST endpoint. */
  lighterBaseUrl: z.url().default('https://api.rh.lighter.xyz'),
});

export const ConfigSchema = z.object({
  /** Robinhood Chain JSON-RPC endpoint. */
  rpcUrl: z.url().default(DEFAULT_RPC_URL),
  chainId: z.number().int().default(CHAIN_ID),
  /** Loopback port for the HTTP API and the UI. */
  port: z.number().int().min(1).max(65_535).default(DEFAULT_PORT),
  /** Bearer token every HTTP route except `GET /v1/health` requires. */
  token: z.string().min(16),
  /**
   * Signed execution. False keeps every order a dry run: it quotes, checks
   * bounds, and records what would have happened.
   */
  live: z.boolean().default(false),
  /** Set by `covenant-desk init --acknowledge-restrictions`. Required before `live`. */
  acknowledgedRestrictions: z.boolean().default(false),
  bounds: BoundsSchema.prefault({}),
  /** How often the recorder writes an observation row, seconds. */
  recorderIntervalSec: z.number().int().min(5).default(60),
  /** Stock tokens the recorder covers, ranked by pool liquidity. */
  recorderTopStocks: z.number().int().min(1).max(200).default(30),
  /** Paired pools the recorder covers, ranked by liquidity. */
  recorderTopPairs: z.number().int().min(1).max(500).default(50),
  /** Start the recorder with the daemon. */
  recorderEnabled: z.boolean().default(true),
  /** How often the order engine evaluates open orders, seconds. */
  engineIntervalSec: z.number().int().min(1).default(5),
  /** How often the registry refreshes the assets API and the Lighter markets, seconds. */
  registryRefreshSec: z.number().int().min(60).default(3600),
  /** Look for pools the desk has not seen yet. */
  poolScanEnabled: z.boolean().default(true),
  /** How often the desk looks for new pools, seconds. */
  poolScanIntervalSec: z.number().int().min(10).default(300),
  /**
   * Blocks per pass of the first scan. The scan records how far it reached
   * after each pass, so a desk that is restarted carries on from there instead
   * of starting over.
   */
  poolScanChunkBlocks: z.number().int().min(100_000).default(5_000_000),
  /** Start the hedge loop with the daemon. */
  hedgeEnabled: z.boolean().default(false),
  hedge: HedgeSchema.prefault({}),
  logLevel: z.enum(['debug', 'info', 'warn', 'error']).default('info'),
});

export type Config = z.infer<typeof ConfigSchema>;
export type Bounds = z.infer<typeof BoundsSchema>;
export type HedgeConfig = z.infer<typeof HedgeSchema>;

/** Absolute paths the desk reads and writes. */
export interface DeskPaths {
  readonly home: string;
  readonly config: string;
  readonly keys: string;
  readonly database: string;
  readonly pidFile: string;
  readonly logFile: string;
}

/** Resolve the desk home directory. `DESK_HOME` wins when it is set. */
export function deskHome(env: NodeJS.ProcessEnv = process.env): string {
  const override = env.DESK_HOME?.trim();
  if (override) return path.resolve(override);
  const xdg = env.XDG_CONFIG_HOME?.trim();
  if (xdg) return path.join(path.resolve(xdg), 'covenant-desk');
  return path.join(homedir(), '.config', 'covenant-desk');
}

/** Every path the desk uses, derived from {@link deskHome}. */
export function deskPaths(env: NodeJS.ProcessEnv = process.env): DeskPaths {
  const home = deskHome(env);
  return {
    home,
    config: path.join(home, 'config.json'),
    keys: path.join(home, 'keys.env'),
    database: path.join(home, 'desk.sqlite'),
    pidFile: path.join(home, 'desk.pid'),
    logFile: path.join(home, 'desk.log'),
  };
}

/** Create the desk home directory if it is missing. Mode 0700. */
export function ensureDeskHome(env: NodeJS.ProcessEnv = process.env): DeskPaths {
  const paths = deskPaths(env);
  mkdirSync(paths.home, { recursive: true, mode: 0o700 });
  return paths;
}

/** A 32-byte bearer token, hex encoded. */
export function generateToken(): string {
  return randomBytes(32).toString('hex');
}

/** Defaults for a fresh install, with a freshly generated bearer token. */
export function defaultConfig(overrides: Partial<Config> = {}): Config {
  return ConfigSchema.parse({ token: generateToken(), ...overrides });
}

/**
 * Read and validate the config file.
 * Throws {@link ConfigError} when the file is missing, unreadable, or invalid.
 */
export function loadConfig(env: NodeJS.ProcessEnv = process.env): Config {
  const paths = deskPaths(env);
  let text: string;
  try {
    text = readFileSync(paths.config, 'utf8');
  } catch {
    throw new ConfigError(`No config at ${paths.config}. Run "covenant-desk init" first.`, { path: paths.config });
  }
  return parseConfig(text, paths.config, env);
}

/** Validate config JSON without touching the filesystem. */
export function parseConfig(
  text: string,
  source = '<memory>',
  env: NodeJS.ProcessEnv = process.env,
): Config {
  let raw: unknown;
  try {
    raw = JSON.parse(text);
  } catch (error) {
    throw new ConfigError(`${source} is not valid JSON: ${(error as Error).message}`, { path: source });
  }
  const parsed = ConfigSchema.safeParse(raw);
  if (!parsed.success) {
    const first = parsed.error.issues[0];
    const where = first && first.path.length > 0 ? first.path.join('.') : 'config';
    throw new ConfigError(`${where} in ${source} is invalid: ${first?.message ?? 'unknown reason'}`, {
      path: source,
      issues: parsed.error.issues,
    });
  }
  return applyEnvOverrides(parsed.data, env);
}

/** Environment overrides, for containers and service units. */
export function applyEnvOverrides(config: Config, env: NodeJS.ProcessEnv = process.env): Config {
  const next: Config = { ...config };
  if (env.DESK_RPC_URL) next.rpcUrl = env.DESK_RPC_URL;
  if (env.DESK_PORT) {
    const port = Number(env.DESK_PORT);
    if (!Number.isInteger(port) || port < 1 || port > 65_535) {
      throw new ConfigError(`DESK_PORT must be a port number, got "${env.DESK_PORT}".`);
    }
    next.port = port;
  }
  if (env.DESK_TOKEN) next.token = env.DESK_TOKEN;
  if (env.DESK_LOG_LEVEL) {
    const level = ConfigSchema.shape.logLevel.safeParse(env.DESK_LOG_LEVEL);
    if (!level.success) throw new ConfigError(`DESK_LOG_LEVEL must be debug, info, warn, or error.`);
    next.logLevel = level.data;
  }
  return next;
}

/** Write the config file with mode 0600. Creates the home directory if needed. */
export function saveConfig(config: Config, env: NodeJS.ProcessEnv = process.env): string {
  const paths = ensureDeskHome(env);
  const validated = ConfigSchema.parse(config);
  writeFileSync(paths.config, `${JSON.stringify(validated, null, 2)}\n`, { mode: 0o600 });
  chmodSync(paths.config, 0o600);
  return paths.config;
}

/**
 * Refuse signed execution unless config allows it and the jurisdiction notice
 * has been acknowledged. Returns the reason when execution is refused.
 */
export function liveBlockedReason(config: Config): string | null {
  if (!config.acknowledgedRestrictions) {
    return 'Restrictions have not been acknowledged. Run "covenant-desk init --acknowledge-restrictions".';
  }
  if (!config.live) return 'The desk is in dry run. Set "live": true in config.json to sign transactions.';
  return null;
}
