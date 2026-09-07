/**
 * The `covenant-desk` command line.
 *
 * Help text is customer-facing copy: it names what each command does and what
 * it costs you to run it. Every command that reads or changes desk state talks
 * to the running desk over its loopback API, so one desk holds the keys and the
 * database and everything else is a client of it.
 */

import { spawn } from 'node:child_process';
import { chmodSync, existsSync, mkdirSync, openSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { homedir } from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import {
  DeskError,
  NotFoundError,
  toErrorResponse,
} from '../../core/errors.js';
import {
  defaultConfig,
  deskPaths,
  ensureDeskHome,
  liveBlockedReason,
  loadConfig,
  saveConfig,
  type Config,
} from '../../core/config.js';
import { openStore } from '../../core/store.js';
import { version } from '../../core/version.js';
import { toRawAmount, type Quantity } from '../../core/types.js';

/** One command, as it appears in `covenant-desk --help`. */
export interface CommandSpec {
  readonly name: string;
  readonly usage: string;
  readonly summary: string;
  /** Every flag the command takes, with its unit and its default. */
  readonly options?: readonly (readonly [flag: string, description: string])[];
  /** Lines printed under the flags, for subcommands and examples. */
  readonly notes?: readonly string[];
}

export const COMMANDS: readonly CommandSpec[] = [
  {
    name: 'init',
    usage: 'init [--acknowledge-restrictions]',
    summary: 'Create the config file, the key file, and the database. Prints where trading is restricted.',
    options: [
      ['--acknowledge-restrictions', 'Record that you have read the jurisdiction notice. Required before live execution.'],
    ],
  },
  {
    name: 'start',
    usage: 'start [--foreground]',
    summary: 'Run the desk: price loops, order engine, API, and page.',
    options: [['--foreground, -f', 'Run in this terminal instead of in the background.']],
  },
  { name: 'stop', usage: 'stop', summary: 'Stop a running desk.' },
  {
    name: 'status',
    usage: 'status',
    summary: 'Show session state, block height, open orders, and dry run or live.',
    options: [['--json', 'Print the raw answer.']],
  },
  {
    name: 'quote',
    usage: 'quote <token> [--usd 100] [--side buy|sell]',
    summary:
      'Price a token on chain, at fair value, and for the size you asked for, with the premium its stock leg carries.',
    options: [
      ['--usd <amount>', 'Size to price, USD. Default 100.'],
      ['--side buy|sell', 'Direction to price. Default buy.'],
      ['--json', 'Print the raw answer.'],
    ],
    notes: ['<token> is a stock symbol such as NVDA, or the address of a token quoted in one.'],
  },
  {
    name: 'premium',
    usage: 'premium [--top 20]',
    summary: 'Rank stock tokens by the gap to their reference price.',
    options: [
      ['--top <count>', 'How many tokens to list, deepest pool first. Default 20.'],
      ['--json', 'Print the raw answer.'],
    ],
  },
  {
    name: 'pools',
    usage: 'pools [--stock NVDA]',
    summary: 'List pools for a stock token, deepest first, with fees.',
    options: [
      ['--stock <symbol>', 'Stock symbol or token address.'],
      ['--include-traps', 'Show pools charging more than 300 bps, which routing refuses.'],
      ['--json', 'Print the raw answer.'],
    ],
  },
  {
    name: 'order',
    usage: 'order create|list|show|cancel',
    summary: 'Work with conditional orders. Orders are dry runs until you turn live execution on.',
    options: [
      ['--kind <kind>', 'limit, stop, takeProfit, atOpen, or premium. Default limit.'],
      ['--side buy|sell', 'Default buy.'],
      ['--token-in <address>', 'Token being sold.'],
      ['--token-out <address>', 'Token being bought.'],
      ['--amount-tokens <amount>', 'Size in whole tokens of --token-in.'],
      ['--amount-in <amount>', 'Size in the smallest units of --token-in.'],
      ['--decimals <n>', 'Decimals of --token-in. Read from the desk when left out.'],
      ['--price-lte <usd>', 'Fire at or below this USD price.'],
      ['--price-gte <usd>', 'Fire at or above this USD price.'],
      ['--basis usdOnchain|usdFair', 'Which price the conditions read. Default usdOnchain.'],
      ['--premium-lte-bps <bps>', 'Fire when the stock leg premium is at or below this.'],
      ['--premium-gte-bps <bps>', 'Fire when the stock leg premium is at or above this.'],
      ['--at <ms>', 'Fire at this time, milliseconds since the Unix epoch.'],
      ['--at-next-open', 'Fire at the next United States regular open.'],
      ['--open-offset-sec <s>', 'Seconds to wait after that open. Default 0.'],
      ['--max-slippage-bps <bps>', 'Slippage cap for this order.'],
      ['--max-notional-usd <usd>', 'Size cap for this order.'],
      ['--max-premium-bps <bps>', 'Buy premium cap for this order.'],
      ['--expires-at <ms>', 'Cancel the order at this time.'],
      ['--live', 'Ask for a signed swap. Needs live execution in the config too.'],
      ['--legs <json>', 'The two sides of an OCO order, as a JSON array. Each leg carries its own conditions.'],
      ['--json', 'Print the raw answer.'],
    ],
    notes: [
      'covenant-desk order create --kind oco --side buy --token-in 0x... --token-out 0x... --amount-tokens 25 \\',
      '  --legs \'[{"side":"buy","tokenIn":"0x...","tokenOut":"0x...","amountIn":"1000000","trigger":{"priceLte":200}},',
      '           {"side":"buy","tokenIn":"0x...","tokenOut":"0x...","amountIn":"1000000","trigger":{"priceGte":300}}]\'',
      'covenant-desk order list [--status open]',
      'covenant-desk order show <id>',
      'covenant-desk order cancel <id>',
    ],
  },
  {
    name: 'hedge',
    usage: 'hedge plan|apply|status|unwind',
    summary: 'Size and hold a short that cancels the stock exposure inside a position.',
    options: [
      ['--symbol <symbol>', 'Stock the position carries, such as NVDA.'],
      ['--usd <amount>', 'Stock exposure to cancel, USD.'],
      ['--qty <amount>', 'Quantity of the paired token held, whole tokens.'],
      ['--ratio <amount>', 'Stock tokens per paired token, from the pool.'],
      ['--json', 'Print the raw answer.'],
    ],
    notes: [
      'covenant-desk hedge plan NVDA --usd 1000',
      'covenant-desk hedge apply NVDA --usd 1000',
      'covenant-desk hedge status',
      'covenant-desk hedge unwind [--symbol NVDA]',
    ],
  },
  {
    name: 'record',
    usage: 'record start|stop|export',
    summary: 'Record on-chain prices against reference prices, and write them to CSV.',
    options: [
      ['--out <file>', 'Where to write the CSV.'],
      ['--hours <n>', 'How far back to export. Default 24.'],
      ['--since <ms>', 'Start of the window, milliseconds since the Unix epoch.'],
      ['--symbol <symbol>', 'Export one stock symbol only.'],
    ],
    notes: ['covenant-desk record export --out premium.csv --hours 24'],
  },
  { name: 'mcp', usage: 'mcp', summary: 'Serve the desk tools over MCP on stdio for an agent to call.' },
  {
    name: 'service',
    usage: 'service install --launchd|--systemd | service uninstall',
    summary: 'Install or remove the background service that keeps the desk running.',
    options: [
      ['--launchd', 'Write a launchd agent. The default on macOS.'],
      ['--systemd', 'Write a systemd user unit. The default on Linux.'],
    ],
  },
];

/** Jurisdiction notice printed by `init`, before live execution can be enabled. */
export const RESTRICTIONS_NOTICE = [
  'Robinhood stock tokens are issued by Robinhood Assets (Jersey) Ltd and are not offered to residents of',
  'the United States, Canada, the United Kingdom, or Switzerland. Check the rules that apply to you before',
  'you trade. The desk holds your keys locally and signs nothing until you turn live execution on.',
].join('\n');

export { version };

/** Full help text. */
export function helpText(): string {
  const width = Math.max(...COMMANDS.map((command) => command.usage.length));
  const lines = COMMANDS.map((command) => `  ${command.usage.padEnd(width)}  ${command.summary}`);
  return [
    'covenant-desk: a local execution desk for Robinhood Chain stock tokens and the tokens paired with them.',
    '',
    'Usage: covenant-desk <command> [options]',
    '',
    'Commands:',
    ...lines,
    '',
    'Options:',
    '  --help      Show this text. Add it after a command for that command\'s flags.',
    '  --version   Show the version.',
    '  --json      Print the raw answer from the desk instead of a table.',
    '',
    'Keys stay on this machine. Orders are dry runs until live execution is turned on in the config file.',
  ].join('\n');
}

/** Usage, flags, and examples for one command. */
export function commandHelp(name: string): string {
  const spec = COMMANDS.find((command) => command.name === name);
  if (!spec) return helpText();
  const lines = [`covenant-desk ${spec.usage}`, '', spec.summary];
  if (spec.options && spec.options.length > 0) {
    const width = Math.max(...spec.options.map(([flag]) => flag.length));
    lines.push('', 'Options:');
    for (const [flag, description] of spec.options) lines.push(`  ${flag.padEnd(width)}  ${description}`);
  }
  if (spec.notes && spec.notes.length > 0) lines.push('', ...spec.notes.map((note) => `  ${note}`));
  return lines.join('\n');
}

/** True when the command line asks for help rather than for work. */
function wantsHelp(args: readonly string[]): boolean {
  return args.includes('--help') || args.includes('-h');
}

export interface CliIo {
  out(text: string): void;
  err(text: string): void;
  readonly argv: readonly string[];
  readonly env: NodeJS.ProcessEnv;
}

/** Command handlers. */
export type CommandHandler = (args: readonly string[], io: CliIo) => Promise<number>;

/** A parsed command line. Values are strings; flags given without a value are `true`. */
export interface ParsedArgs {
  readonly positionals: readonly string[];
  readonly flags: Readonly<Record<string, string | boolean>>;
}

/**
 * Flags that never take a value. Without this list `--json NVDA` would read the
 * symbol as the value of `--json` and leave the command with no token.
 */
const BOOLEAN_FLAGS = new Set([
  'json',
  'help',
  'h',
  'version',
  'v',
  'live',
  'foreground',
  'f',
  'include-traps',
  'at-next-open',
  'acknowledge-restrictions',
  'launchd',
  'systemd',
]);

/**
 * Parse `--flag value`, `--flag=value`, `--flag`, and `--no-flag`. Everything
 * else is a positional. `--` stops option parsing.
 */
export function parseArgs(argv: readonly string[]): ParsedArgs {
  const positionals: string[] = [];
  const flags: Record<string, string | boolean> = {};
  let literal = false;

  for (let i = 0; i < argv.length; i += 1) {
    const arg = argv[i] ?? '';
    if (literal || !arg.startsWith('-') || arg === '-') {
      positionals.push(arg);
      continue;
    }
    if (arg === '--') {
      literal = true;
      continue;
    }
    const body = arg.startsWith('--') ? arg.slice(2) : arg.slice(1);
    const equals = body.indexOf('=');
    if (equals > 0) {
      flags[body.slice(0, equals)] = body.slice(equals + 1);
      continue;
    }
    if (body.startsWith('no-')) {
      flags[body.slice(3)] = false;
      continue;
    }
    const next = argv[i + 1];
    if (!BOOLEAN_FLAGS.has(body) && next !== undefined && !next.startsWith('--')) {
      flags[body] = next;
      i += 1;
    } else {
      flags[body] = true;
    }
  }

  return { positionals, flags };
}

function flagString(parsed: ParsedArgs, ...names: string[]): string | undefined {
  for (const name of names) {
    const value = parsed.flags[name];
    if (typeof value === 'string') return value;
  }
  return undefined;
}

function flagBool(parsed: ParsedArgs, ...names: string[]): boolean {
  for (const name of names) {
    const value = parsed.flags[name];
    if (value === true || value === 'true') return true;
  }
  return false;
}

/** True, false, or absent. `--flag` is true, `--no-flag` is false. */
function flagTri(parsed: ParsedArgs, name: string): boolean | undefined {
  const value = parsed.flags[name];
  if (value === undefined) return undefined;
  if (value === false || value === 'false') return false;
  return true;
}

function flagNumber(parsed: ParsedArgs, ...names: string[]): number | undefined {
  const raw = flagString(parsed, ...names);
  if (raw === undefined) return undefined;
  const value = Number(raw);
  if (!Number.isFinite(value)) throw new DeskError('config_invalid', `--${names[0]} needs a number, got "${raw}".`);
  return value;
}

/** Print a usage problem and ask for exit code 2. */
function usage(io: CliIo, message: string): number {
  io.err(message);
  return 2;
}

interface Api {
  readonly config: Config;
  readonly baseUrl: string;
  get<T>(path: string): Promise<T>;
  post<T>(path: string, body?: unknown): Promise<T>;
  del<T>(path: string): Promise<T>;
}

/** A client of the running desk. Every command that reads state goes through it. */
export function deskApi(env: NodeJS.ProcessEnv, config?: Config): Api {
  const resolved = config ?? loadConfig(env);
  const baseUrl = `http://127.0.0.1:${resolved.port}`;

  const call = async <T,>(method: string, route: string, body?: unknown): Promise<T> => {
    let response: Response;
    try {
      response = await fetch(`${baseUrl}${route}`, {
        method,
        headers: {
          authorization: `Bearer ${resolved.token}`,
          ...(body === undefined ? {} : { 'content-type': 'application/json' }),
        },
        body: body === undefined ? undefined : JSON.stringify(body),
      });
    } catch {
      throw new DeskError(
        'upstream_failed',
        `No desk is answering on 127.0.0.1:${resolved.port}. Start one with "covenant-desk start".`,
        { port: resolved.port },
      );
    }
    const text = await response.text();
    const parsed = text.trim() === '' ? {} : (JSON.parse(text) as Record<string, unknown>);
    if (!response.ok) {
      const code = typeof parsed.error === 'string' ? parsed.error : 'upstream_failed';
      const reason = typeof parsed.reason === 'string' ? parsed.reason : `The desk answered ${response.status}.`;
      throw new DeskError(code as DeskError['code'], reason);
    }
    return parsed as T;
  };

  return {
    config: resolved,
    baseUrl,
    get: (route) => call('GET', route),
    post: (route, body) => call('POST', route, body ?? {}),
    del: (route) => call('DELETE', route),
  };
}

function pageLink(config: Config): string {
  return `http://127.0.0.1:${config.port}/?token=${config.token}`;
}

function money(quantity: Quantity | undefined, digits = 2): string {
  if (!quantity || typeof quantity.value !== 'number' || !Number.isFinite(quantity.value)) return '';
  const value = quantity.value;
  const size = Math.abs(value);
  if (size >= 10) return value.toFixed(digits);
  if (size >= 0.01 || size === 0) return value.toFixed(Math.max(digits, 4));
  // A token paired with a stock often trades below a cent, so a fixed number of
  // decimal places would print every one of them as zero.
  return value.toFixed(Math.min(18, Math.ceil(-Math.log10(size)) + 3));
}

function bps(quantity: Quantity | undefined): string {
  if (!quantity || typeof quantity.value !== 'number') return '';
  return `${quantity.value > 0 ? '+' : ''}${quantity.value.toFixed(0)}`;
}

/** Longest symbol a table cell shows before it is cut. */
const MAX_SYMBOL_WIDTH = 12;

/**
 * A token symbol as it is safe to print.
 *
 * Symbols on chain 4663 are whatever the deployer wrote: the desk has seen one
 * 242 characters long, and nothing stops one holding control characters. A
 * table has to stay readable, so anything unprintable goes and the rest is cut.
 */
export function displaySymbol(symbol: string | undefined, fallback = ''): string {
  const cleaned = (symbol ?? '')
    .replace(/[^\x20-\x7e]+/g, ' ')
    .replace(/\s+/g, ' ')
    .trim();
  if (cleaned === '') return fallback;
  return cleaned.length > MAX_SYMBOL_WIDTH ? `${cleaned.slice(0, MAX_SYMBOL_WIDTH - 1)}\u2026` : cleaned;
}

/** Fixed-width table. Numeric columns are right aligned by the caller. */
function table(header: readonly string[], rows: readonly (readonly string[])[]): string {
  const widths = header.map((cell, index) =>
    Math.max(cell.length, ...rows.map((row) => (row[index] ?? '').length)),
  );
  const line = (cells: readonly string[]) =>
    cells.map((cell, index) => cell.padEnd(widths[index] ?? cell.length)).join('  ').trimEnd();
  return [line(header), line(widths.map((width) => '-'.repeat(width))), ...rows.map(line)].join('\n');
}

function printJson(io: CliIo, value: unknown): number {
  io.out(JSON.stringify(value, (_key, item) => (typeof item === 'bigint' ? item.toString() : item), 2));
  return 0;
}

function cliMain(): string {
  return path.join(path.dirname(fileURLToPath(import.meta.url)), 'main.js');
}

/** Locate the service templates, whether the package is built or run from source. */
function serviceDir(): string {
  const here = path.dirname(fileURLToPath(import.meta.url));
  for (const candidate of ['../../../service', '../../../../service']) {
    const dir = path.resolve(here, candidate);
    if (existsSync(path.join(dir, 'com.opencovenant.desk.plist'))) return dir;
  }
  throw new NotFoundError('The service templates', { looked: here });
}

function pidOf(env: NodeJS.ProcessEnv): number | undefined {
  const file = deskPaths(env).pidFile;
  if (!existsSync(file)) return undefined;
  const pid = Number(readFileSync(file, 'utf8').trim());
  if (!Number.isInteger(pid) || pid <= 0) return undefined;
  try {
    process.kill(pid, 0);
    return pid;
  } catch {
    return undefined;
  }
}

const sleep = (ms: number) => new Promise((resolve) => setTimeout(resolve, ms));

async function waitForHealth(config: Config, timeoutMs: number): Promise<boolean> {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    try {
      const response = await fetch(`http://127.0.0.1:${config.port}/v1/health`);
      if (response.ok) return true;
    } catch {
      // The desk has not opened its port yet.
    }
    await sleep(250);
  }
  return false;
}

const KEYS_TEMPLATE = [
  '# Keys for Covenant Desk. This file is read once at start and never leaves the machine.',
  '# Keep it at mode 600. Leave a line blank to run without that key.',
  '',
  'DESK_EVM_PRIVATE_KEY=',
  'DESK_LIGHTER_RH_PRIVATE_KEY=',
  'DESK_LIGHTER_RH_ACCOUNT_INDEX=',
  'DESK_LIGHTER_RH_API_KEY_INDEX=',
  '',
].join('\n');

const init: CommandHandler = async (args, io) => {
  if (wantsHelp(args)) {
    io.out(commandHelp('init'));
    return 0;
  }
  const parsed = parseArgs(args);
  const acknowledged = flagBool(parsed, 'acknowledge-restrictions');
  const paths = ensureDeskHome(io.env);
  const existed = existsSync(paths.config);

  const config = existed
    ? { ...loadConfig(io.env), acknowledgedRestrictions: acknowledged || loadConfig(io.env).acknowledgedRestrictions }
    : defaultConfig({ acknowledgedRestrictions: acknowledged });
  saveConfig(config, io.env);

  if (!existsSync(paths.keys)) {
    writeFileSync(paths.keys, KEYS_TEMPLATE, { mode: 0o600 });
    chmodSync(paths.keys, 0o600);
  }
  openStore({ path: paths.database }).close();

  io.out(RESTRICTIONS_NOTICE);
  io.out('');
  io.out(existed ? `Kept your settings in ${paths.config}.` : `Wrote ${paths.config}.`);
  io.out(`Keys go in ${paths.keys}, mode 600. The database is ${paths.database}.`);
  io.out(`Start the desk with "covenant-desk start", then open ${pageLink(config)}`);
  io.out('');
  io.out(
    config.acknowledgedRestrictions
      ? 'Restrictions acknowledged. Set "live": true in the config file when you want the desk to sign transactions.'
      : 'Every order stays a dry run until you run "covenant-desk init --acknowledge-restrictions".',
  );
  return 0;
};

const start: CommandHandler = async (args, io) => {
  if (wantsHelp(args)) {
    io.out(commandHelp('start'));
    return 0;
  }
  const parsed = parseArgs(args);
  const config = loadConfig(io.env);
  const paths = ensureDeskHome(io.env);

  const running = pidOf(io.env);
  if (running) {
    io.err(`A desk is already running with process id ${running}. Stop it with "covenant-desk stop".`);
    return 1;
  }

  if (flagBool(parsed, 'foreground', 'f')) {
    const { runDaemon } = await import('../../daemon.js');
    const desk = await runDaemon({ config, env: io.env });
    writeFileSync(paths.pidFile, `${process.pid}\n`, { mode: 0o600 });
    const clean = () => {
      try {
        rmSync(paths.pidFile, { force: true });
      } catch {
        // The file is already gone.
      }
    };
    process.once('exit', clean);
    process.once('SIGINT', clean);
    process.once('SIGTERM', clean);
    io.out(`The desk is running. Open ${pageLink(desk.config)}`);
    await new Promise(() => {});
    return 0;
  }

  const log = openSync(paths.logFile, 'a');
  const child = spawn(process.execPath, [cliMain(), 'start', '--foreground'], {
    detached: true,
    stdio: ['ignore', log, log],
    env: { ...process.env, ...io.env },
  });
  child.unref();

  if (!(await waitForHealth(config, 20_000))) {
    io.err(`The desk did not answer on 127.0.0.1:${config.port} within 20 seconds. Look at ${paths.logFile}.`);
    return 1;
  }
  io.out(`The desk is running on 127.0.0.1:${config.port}.`);
  io.out(`Open ${pageLink(config)}`);
  io.out(`Logs are in ${paths.logFile}.`);
  return 0;
};

const stop: CommandHandler = async (args, io) => {
  if (wantsHelp(args)) {
    io.out(commandHelp('stop'));
    return 0;
  }
  const paths = deskPaths(io.env);
  const pid = pidOf(io.env);
  if (!pid) {
    rmSync(paths.pidFile, { force: true });
    io.out('No desk is running.');
    return 0;
  }
  process.kill(pid, 'SIGTERM');
  for (let i = 0; i < 40; i += 1) {
    await sleep(250);
    try {
      process.kill(pid, 0);
    } catch {
      rmSync(paths.pidFile, { force: true });
      io.out(`Stopped the desk that was running with process id ${pid}.`);
      return 0;
    }
  }
  io.err(`The desk with process id ${pid} is still running ten seconds after it was asked to stop.`);
  return 1;
};

const status: CommandHandler = async (args, io) => {
  if (wantsHelp(args)) {
    io.out(commandHelp('status'));
    return 0;
  }
  const parsed = parseArgs(args);
  const api = deskApi(io.env);
  const body = await api.get<Record<string, unknown>>('/v1/status');
  if (flagBool(parsed, 'json')) return printJson(io, body);

  const blocked = liveBlockedReason(api.config);
  const rows: string[][] = [
    ['Version', String(body.version ?? '')],
    ['Chain', String(body.chainId ?? '')],
    ['Block', body.blockNumber ? String(body.blockNumber) : 'not read yet'],
    ['Session', String(body.sessionState ?? '')],
    ['Mode', blocked ? `dry run (${blocked})` : 'live execution'],
    ['Running for', `${Number(body.uptimeSec ?? 0)} s`],
    ['Tokens tracked', String(body.tokensTracked ?? 0)],
    ['Pools tracked', String(body.poolsTracked ?? 0)],
    [
      'Pool scan',
      body.poolScanRunning
        ? `looking for new pools, at block ${body.poolScanBlock ?? 'the start of the chain'}`
        : body.poolScanBlock
          ? `up to date through block ${body.poolScanBlock}`
          : 'not started',
    ],
    ['Open orders', String(body.openOrders ?? 0)],
    ['Observations 24h', String(body.observations24h ?? 0)],
    ['Recorder', body.recorderRunning ? 'running' : 'stopped'],
  ];
  io.out(table(['Field', 'Value'], rows));
  const degraded = Array.isArray(body.degraded) ? (body.degraded as { component: string; reason: string }[]) : [];
  for (const item of degraded) io.err(`${item.component} is not running: ${item.reason}`);
  io.out('');
  io.out(`Page: ${pageLink(api.config)}`);
  return 0;
};

const quote: CommandHandler = async (args, io) => {
  if (wantsHelp(args)) {
    io.out(commandHelp('quote'));
    return 0;
  }
  const parsed = parseArgs(args);
  const token = parsed.positionals[0] ?? flagString(parsed, 'token');
  if (!token) return usage(io, 'Name the token: covenant-desk quote NVDA --usd 100');
  const amountUsd = flagNumber(parsed, 'usd', 'amount-usd') ?? 100;
  const side = flagString(parsed, 'side') ?? 'buy';
  if (side !== 'buy' && side !== 'sell') return usage(io, 'Side is buy or sell.');

  const api = deskApi(io.env);
  const body = await api.get<Record<string, unknown>>(
    `/v1/quote?token=${encodeURIComponent(token)}&amountUsd=${amountUsd}&side=${side}`,
  );
  if (flagBool(parsed, 'json')) return printJson(io, body);

  if (body.kind === 'stock') {
    const fair = (body.fairValue ?? {}) as Record<string, Quantity | string | undefined>;
    io.out(
      table(
        ['Field', 'Value'],
        [
          ['Symbol', displaySymbol(fair.symbol as string | undefined, token)],
          ['On chain USD', money(fair.onchainMid as Quantity)],
          ['Fair value USD', money(fair.reference as Quantity)],
          [`${side === 'sell' ? 'Sell' : 'Buy'} ${amountUsd} USD at`, money(body.sizedPrice as Quantity)],
          ['Reference', String(fair.referenceSource ?? 'none')],
          ['Premium bps', bps(fair.premiumBps as Quantity)],
          ['Session', String(fair.sessionState ?? '')],
          ...(body.note ? [['Note', String(body.note)]] : []),
        ],
      ),
    );
    return 0;
  }

  const paired = (body.quote ?? {}) as Record<string, Quantity | string | undefined>;
  const entry = paired.bestEntryRoute as RouteView | undefined;
  const exit = paired.bestExitRoute as RouteView | undefined;
  const viaWeth = paired.usdViaWeth as Quantity | undefined;
  io.out(
    table(
      ['Field', 'Value'],
      [
        ['Token', displaySymbol(paired.symbol as string | undefined, token)],
        ['Quoted in', displaySymbol(paired.stockSymbol as string | undefined)],
        ['On chain USD', money(paired.usdOnchain as Quantity)],
        ['Fair value USD', money(paired.usdFair as Quantity)],
        ['Stock leg premium bps', bps(paired.stockLegPremiumBps as Quantity)],
        ...(viaWeth ? [['Through ether USD', money(viaWeth)]] : []),
        ['Best way in USD', money(entry?.usdPrice)],
        ...(entry?.note ? [['Way in', entry.note]] : []),
        ['Best way out USD', money(exit?.usdPrice)],
        ...(exit?.note ? [['Way out', exit.note]] : []),
      ],
    ),
  );
  return 0;
};

/** The part of a route summary the table prints. */
interface RouteView {
  readonly usdPrice?: Quantity;
  readonly note?: string;
}

const premium: CommandHandler = async (args, io) => {
  if (wantsHelp(args)) {
    io.out(commandHelp('premium'));
    return 0;
  }
  const parsed = parseArgs(args);
  const top = flagNumber(parsed, 'top', 'limit') ?? 20;
  const api = deskApi(io.env);
  const body = await api.get<{ premium?: Record<string, Quantity | string | undefined>[] }>(`/v1/premium?limit=${top}`);
  if (flagBool(parsed, 'json')) return printJson(io, body);

  const rows = (body.premium ?? []).map((row) => [
    displaySymbol(row.symbol as string | undefined),
    money(row.onchainMid as Quantity),
    money(row.reference as Quantity),
    String(row.referenceSource ?? ''),
    bps(row.premiumBps as Quantity),
    String(row.sessionState ?? ''),
  ]);
  if (rows.length === 0) {
    io.out('No stock token has a priced pool yet.');
    return 0;
  }
  io.out(table(['Symbol', 'On chain', 'Reference', 'Source', 'Premium bps', 'Session'], rows));
  return 0;
};

const pools: CommandHandler = async (args, io) => {
  if (wantsHelp(args)) {
    io.out(commandHelp('pools'));
    return 0;
  }
  const parsed = parseArgs(args);
  const stock = flagString(parsed, 'stock', 'token') ?? parsed.positionals[0];
  if (!stock) return usage(io, 'Name the stock token: covenant-desk pools --stock NVDA');
  const api = deskApi(io.env);
  const body = await api.get<{ pools?: Record<string, unknown>[]; names?: Record<string, string> }>(
    `/v1/pools?stock=${encodeURIComponent(stock)}&includeTraps=${flagBool(parsed, 'include-traps')}`,
  );
  if (flagBool(parsed, 'json')) return printJson(io, body);

  const names = body.names ?? {};
  const named = (address: unknown): string => {
    const key = String(address ?? '').toLowerCase();
    return displaySymbol(names[key], `${key.slice(0, 10)}...`);
  };
  const rows = (body.pools ?? []).map((pool) => [
    String(pool.poolId ?? '').slice(0, 12),
    named(pool.currency0),
    named(pool.currency1),
    pool.lpFee === undefined || pool.lpFee === null ? '' : String(Number(pool.lpFee) / 100),
    String(pool.liquidity ?? ''),
    pool.trap ? 'above the routing limit' : '',
  ]);
  if (rows.length === 0) {
    io.out(`No pool quotes ${stock} yet.`);
    return 0;
  }
  io.out(table(['Pool', 'Currency 0', 'Currency 1', 'Fee bps', 'Liquidity', 'Note'], rows));
  return 0;
};

const order: CommandHandler = async (args, io) => {
  if (wantsHelp(args)) {
    io.out(commandHelp('order'));
    return 0;
  }
  const [sub, ...rest] = args;
  const parsed = parseArgs(rest);
  if (sub !== undefined && !['create', 'list', 'show', 'cancel'].includes(sub)) {
    return usage(io, 'Use: covenant-desk order create|list|show|cancel');
  }

  if (sub === 'create') {
    const kind = flagString(parsed, 'kind') ?? 'limit';
    const side = flagString(parsed, 'side') ?? 'buy';
    const tokenIn = flagString(parsed, 'token-in');
    const tokenOut = flagString(parsed, 'token-out');
    if (!tokenIn || !tokenOut) {
      return usage(
        io,
        'Give both sides as addresses: covenant-desk order create --token-in 0x... --token-out 0x... --amount-tokens 25 --price-lte 180',
      );
    }
    const amountRaw = flagString(parsed, 'amount-in');
    const amountTokens = flagString(parsed, 'amount-tokens');
    if (!amountRaw && !amountTokens) {
      return usage(io, 'Give the size: --amount-tokens 25, or --amount-in in smallest units.');
    }
    const api = deskApi(io.env);
    // The desk knows what a token's decimals are, from its own records or from
    // the token itself, so --amount-tokens does not depend on the caller
    // remembering that USDG carries six of them.
    const decimals = amountTokens
      ? (flagNumber(parsed, 'decimals') ?? (await tokenDecimals(api, tokenIn)))
      : undefined;
    if (amountTokens && decimals === undefined) {
      return usage(
        io,
        `The desk does not know how many decimals ${tokenIn} has, so --amount-tokens cannot be converted. Give the size with --amount-in in smallest units, or pass --decimals.`,
      );
    }
    const trigger: Record<string, unknown> = {};
    const priceLte = flagNumber(parsed, 'price-lte');
    const priceGte = flagNumber(parsed, 'price-gte');
    const premiumLte = flagNumber(parsed, 'premium-lte-bps');
    const premiumGte = flagNumber(parsed, 'premium-gte-bps');
    const at = flagNumber(parsed, 'at');
    if (priceLte !== undefined) trigger.priceLte = priceLte;
    if (priceGte !== undefined) trigger.priceGte = priceGte;
    if (premiumLte !== undefined) trigger.premiumLteBps = premiumLte;
    if (premiumGte !== undefined) trigger.premiumGteBps = premiumGte;
    if (at !== undefined) trigger.at = at;
    if (flagBool(parsed, 'at-next-open')) trigger.atNextOpenOffsetSec = flagNumber(parsed, 'open-offset-sec') ?? 0;
    const basis = flagString(parsed, 'basis');
    if (basis) trigger.priceBasis = basis;

    const bounds: Record<string, number> = {};
    const slippage = flagNumber(parsed, 'max-slippage-bps');
    const notional = flagNumber(parsed, 'max-notional-usd');
    const premiumCap = flagNumber(parsed, 'max-premium-bps');
    if (slippage !== undefined) bounds.maxSlippageBps = slippage;
    if (notional !== undefined) bounds.maxOrderNotionalUsd = notional;
    if (premiumCap !== undefined) bounds.maxBuyPremiumBps = premiumCap;

    let legs: unknown;
    const legsFlag = flagString(parsed, 'legs');
    if (legsFlag !== undefined) {
      try {
        legs = JSON.parse(legsFlag);
      } catch (error) {
        return usage(io, `--legs needs a JSON array of two legs: ${(error as Error).message}`);
      }
    }

    const body = await api.post<{ orders?: Record<string, unknown>[] }>('/v1/orders', {
      kind,
      side,
      tokenIn,
      tokenOut,
      ...(legs === undefined ? {} : { legs }),
      ...(amountRaw
        ? { amountIn: amountRaw }
        : { amountIn: toRawAmount(amountTokens ?? '0', decimals ?? 18).toString() }),
      trigger,
      ...(Object.keys(bounds).length > 0 ? { bounds } : {}),
      live: flagBool(parsed, 'live'),
      ...(flagNumber(parsed, 'expires-at') === undefined ? {} : { expiresAt: flagNumber(parsed, 'expires-at') }),
    });
    if (flagBool(parsed, 'json')) return printJson(io, body);
    for (const created of body.orders ?? []) {
      io.out(`${created.id} ${created.kind} ${created.side} ${created.status}${created.live ? ' live' : ' dry run'}`);
    }
    return 0;
  }

  if (sub === 'list' || sub === undefined) {
    const api = deskApi(io.env);
    const state = flagString(parsed, 'status');
    const body = await api.get<{ orders?: Record<string, unknown>[] }>(
      `/v1/orders${state ? `?status=${encodeURIComponent(state)}` : ''}`,
    );
    if (flagBool(parsed, 'json')) return printJson(io, body);
    const rows = (body.orders ?? []).map((row) => [
      String(row.id ?? '').slice(0, 8),
      String(row.kind ?? ''),
      String(row.side ?? ''),
      String(row.amountIn ?? ''),
      String(row.status ?? ''),
      row.live ? 'live' : 'dry run',
      String(row.reason ?? ''),
    ]);
    if (rows.length === 0) {
      io.out('No orders yet.');
      return 0;
    }
    io.out(table(['Id', 'Kind', 'Side', 'Size in', 'Status', 'Mode', 'Reason'], rows));
    return 0;
  }

  if (sub === 'show') {
    const id = parsed.positionals[0];
    if (!id) return usage(io, 'Name the order: covenant-desk order show <id>');
    const api = deskApi(io.env);
    const body = await api.get<{
      order?: Record<string, unknown>;
      executions?: Record<string, unknown>[];
    }>(`/v1/orders/${encodeURIComponent(id)}`);
    if (flagBool(parsed, 'json')) return printJson(io, body);

    const order = body.order ?? {};
    io.out(
      table(
        ['Field', 'Value'],
        [
          ['Order', String(order.id ?? id)],
          ['Kind', `${String(order.kind ?? '')} ${String(order.side ?? '')}`.trim()],
          ['Status', String(order.status ?? '')],
          ['Mode', order.live ? 'live' : 'dry run'],
          ['Size in', String(order.amountIn ?? '')],
          ...(order.reason ? [['Reason', String(order.reason)]] : []),
        ],
      ),
    );

    const fills = body.executions ?? [];
    if (fills.length === 0) {
      io.out('');
      io.out('Nothing has filled against this order yet.');
      return 0;
    }
    io.out('');
    io.out(
      table(
        ['When', 'Mode', 'Size in', 'Size out', 'Quoted out', 'Out per in', 'USD', 'Slippage bps', 'Status'],
        fills.map((fill) => [
          new Date(Number(fill.createdAt ?? 0)).toISOString(),
          fill.live ? 'live' : 'dry run',
          String(fill.amountIn ?? ''),
          String(fill.amountOut ?? ''),
          String(fill.quotedAmountOut ?? ''),
          money(fill.effectivePrice as Quantity),
          money(fill.notionalUsd as Quantity),
          String(Math.round(Number(fill.slippageBps ?? 0))),
          String(fill.status ?? ''),
        ]),
      ),
    );
    const reasons = fills.map((fill) => fill.reason).filter(Boolean);
    for (const reason of reasons) io.out(String(reason));
    return 0;
  }

  if (sub === 'cancel') {
    const id = parsed.positionals[0];
    if (!id) return usage(io, 'Name the order: covenant-desk order cancel <id>');
    const api = deskApi(io.env);
    const body = await api.del<{ order?: Record<string, unknown> }>(`/v1/orders/${encodeURIComponent(id)}`);
    if (flagBool(parsed, 'json')) return printJson(io, body);
    io.out(`Order ${id} is ${String(body.order?.status ?? 'cancelled')}.`);
    return 0;
  }

  return usage(io, 'Use: covenant-desk order create|list|show|cancel');
};

const hedge: CommandHandler = async (args, io) => {
  if (wantsHelp(args)) {
    io.out(commandHelp('hedge'));
    return 0;
  }
  const [sub, ...rest] = args;
  const parsed = parseArgs(rest);
  if (sub !== undefined && !['plan', 'apply', 'status', 'unwind'].includes(sub)) {
    return usage(io, 'Use: covenant-desk hedge plan|apply|status|unwind');
  }

  const planQuery = () => {
    const symbol = flagString(parsed, 'symbol') ?? parsed.positionals[0];
    if (!symbol) return undefined;
    const query = new URLSearchParams({ symbol });
    const usd = flagNumber(parsed, 'usd', 'stock-leg-usd');
    const qty = flagNumber(parsed, 'qty');
    const ratio = flagNumber(parsed, 'ratio');
    if (usd !== undefined) query.set('stockLegUsd', String(usd));
    if (qty !== undefined) query.set('qty', String(qty));
    if (ratio !== undefined) query.set('ratio', String(ratio));
    return query;
  };

  if (sub === 'plan' || sub === 'apply') {
    const query = planQuery();
    if (!query) return usage(io, 'Name the stock: covenant-desk hedge plan NVDA --usd 1000');
    const api = deskApi(io.env);
    const body =
      sub === 'plan'
        ? await api.get<{ plan?: Record<string, unknown> }>(`/v1/hedge/plan?${query.toString()}`)
        : await api.post<{ plan?: Record<string, unknown> }>('/v1/hedge/apply', Object.fromEntries(
            [...query.entries()].map(([key, value]) => [key, key === 'symbol' ? value : Number(value)]),
          ));
    if (flagBool(parsed, 'json')) return printJson(io, body);
    const plan = (body.plan ?? {}) as Record<string, Quantity | string | boolean | undefined>;
    io.out(
      table(
        ['Field', 'Value'],
        [
          ['Symbol', String(plan.symbol ?? '')],
          ['Stock exposure USD', money(plan.stockLegNotionalUsd as Quantity)],
          ['Reference price USD', money(plan.referencePrice as Quantity)],
          ['Short to hold', money(plan.targetShortBase as Quantity, 4)],
          ['Held now', money(plan.currentShortBase as Quantity, 4)],
          ['Next step', String(plan.action ?? '')],
          ['Funding 8h bps', bps(plan.fundingBps8h as Quantity)],
          ['Can be sent', plan.executable ? 'yes' : 'no'],
          ['Reason', String(plan.reason ?? '')],
        ],
      ),
    );
    return 0;
  }

  if (sub === 'status' || sub === undefined) {
    const api = deskApi(io.env);
    const body = await api.get<{ positions?: Record<string, Quantity | string>[]; reason?: string }>('/v1/hedge');
    if (flagBool(parsed, 'json')) return printJson(io, body);
    const rows = (body.positions ?? []).map((position) => [
      displaySymbol(position.symbol as string | undefined),
      money(position.sizeBase as Quantity, 4),
      money(position.markPrice as Quantity),
      bps(position.fundingBps8h as Quantity),
    ]);
    if (rows.length === 0) {
      io.out(body.reason ?? 'No hedge is open.');
      return 0;
    }
    if (body.reason) io.err(body.reason);
    io.out(table(['Symbol', 'Size', 'Mark', 'Funding 8h bps'], rows));
    return 0;
  }

  if (sub === 'unwind') {
    const symbol = flagString(parsed, 'symbol') ?? parsed.positionals[0];
    const api = deskApi(io.env);
    const body = await api.post<{ plans?: Record<string, unknown>[] }>('/v1/hedge/unwind', { symbol });
    if (flagBool(parsed, 'json')) return printJson(io, body);
    const plans = body.plans ?? [];
    if (plans.length === 0) {
      io.out('There was nothing to unwind.');
      return 0;
    }
    const sentPlans = plans.filter((entry) => entry.executable === true);
    const refused = plans.filter((entry) => entry.executable !== true);
    if (sentPlans.length > 0) {
      io.out(
        `Sent ${sentPlans.length} closing order(s): ${sentPlans
          .map((entry) => displaySymbol(entry.symbol as string | undefined))
          .join(', ')}.`,
      );
    }
    for (const entry of refused) {
      io.err(
        `${displaySymbol(entry.symbol as string | undefined, 'A position')} was not closed: ${String(
          entry.reason ?? 'the venue did not accept the order',
        )}`,
      );
    }
    return sentPlans.length === 0 ? 1 : 0;
  }

  return usage(io, 'Use: covenant-desk hedge plan|apply|status|unwind');
};

const record: CommandHandler = async (args, io) => {
  if (wantsHelp(args)) {
    io.out(commandHelp('record'));
    return 0;
  }
  const [sub, ...rest] = args;
  const parsed = parseArgs(rest);
  if (sub !== 'start' && sub !== 'stop' && sub !== 'export') {
    return usage(io, 'Use: covenant-desk record start|stop|export --out premium.csv');
  }

  if (sub === 'start' || sub === 'stop') {
    const api = deskApi(io.env);
    const body = await api.post<{ running?: boolean; intervalSec?: number }>(`/v1/recorder/${sub}`);
    io.out(
      body.running
        ? `Recording every ${body.intervalSec ?? api.config.recorderIntervalSec} seconds.`
        : 'Recording is stopped.',
    );
    return 0;
  }

  if (sub === 'export') {
    const out = flagString(parsed, 'out', 'o') ?? parsed.positionals[0];
    if (!out) return usage(io, 'Name the file: covenant-desk record export --out premium.csv');
    const hours = flagNumber(parsed, 'hours') ?? 24;
    const since = flagNumber(parsed, 'since') ?? Date.now() - hours * 3600_000;
    const symbol = flagString(parsed, 'symbol');
    const api = deskApi(io.env);
    const rows = await everyObservation(api, { since: Math.round(since), ...(symbol ? { symbol } : {}) });
    const header = [
      'ts',
      'iso',
      'symbol',
      'token',
      'pool',
      'onchainMidUsd',
      'referenceUsd',
      'referenceSource',
      'premiumBps',
      'sessionState',
      'blockNumber',
      'stockSymbol',
      'liquidity',
    ];
    const csv = [
      header.join(','),
      ...rows.map((row) =>
        header
          .map((field) => (field === 'iso' ? new Date(Number(row.ts)).toISOString() : csvCell(row[field])))
          .join(','),
      ),
    ].join('\n');
    const target = path.resolve(out);
    mkdirSync(path.dirname(target), { recursive: true });
    writeFileSync(target, `${csv}\n`);
    io.out(`Wrote ${rows.length} observations to ${target}.`);
    return 0;
  }

  return usage(io, 'Use: covenant-desk record start|stop|export --out premium.csv');
};

/** Rows one export request asks for. The desk caps a single read well above this. */
const EXPORT_PAGE = 5_000;

/**
 * Every observation in the window, read a page at a time.
 *
 * The recorder writes tens of thousands of rows a day, so one request would
 * hit the desk's own cap and the file would quietly hold part of the window.
 * Paging by timestamp keeps whole passes together, and rows already seen are
 * dropped because several observations share a millisecond.
 */
async function everyObservation(
  api: Api,
  filter: { since: number; symbol?: string },
): Promise<Record<string, unknown>[]> {
  const rows: Record<string, unknown>[] = [];
  const seen = new Set<number>();
  let since = filter.since;

  for (;;) {
    const query = new URLSearchParams({ since: String(since), limit: String(EXPORT_PAGE) });
    if (filter.symbol) query.set('symbol', filter.symbol);
    const body = await api.get<{ observations?: Record<string, unknown>[] }>(`/v1/recorder?${query.toString()}`);
    const page = body.observations ?? [];
    let added = 0;
    for (const row of page) {
      const id = Number(row.id);
      if (Number.isFinite(id)) {
        if (seen.has(id)) continue;
        seen.add(id);
      }
      rows.push(row);
      added += 1;
    }
    if (page.length < EXPORT_PAGE) return rows;

    const last = page[page.length - 1];
    const lastTs = Number(last?.ts ?? since);
    // A page entirely inside one millisecond cannot be advanced past without
    // losing rows, so the export stops rather than skipping them.
    since = lastTs > since ? lastTs : lastTs + 1;
    if (added === 0) return rows;
  }
}

/** Decimals the desk holds for a token, from its own records or from chain. */
async function tokenDecimals(api: Api, token: string): Promise<number | undefined> {
  try {
    const body = await api.get<{ token?: { decimals?: number } }>(`/v1/tokens/${encodeURIComponent(token)}`);
    const decimals = body.token?.decimals;
    return typeof decimals === 'number' ? decimals : undefined;
  } catch {
    return undefined;
  }
}

function csvCell(value: unknown): string {
  if (value === null || value === undefined) return '';
  const text = String(value);
  return /[",\n]/.test(text) ? `"${text.replace(/"/g, '""')}"` : text;
}

const mcp: CommandHandler = async (args, io) => {
  if (wantsHelp(args)) {
    io.out(commandHelp('mcp'));
    return 0;
  }
  const { createMcpSurface } = await import('../mcp/index.js');
  const config = loadConfig(io.env);
  await createMcpSurface({ config }).serve();
  return 0;
};

const service: CommandHandler = async (args, io) => {
  if (wantsHelp(args)) {
    io.out(commandHelp('service'));
    return 0;
  }
  const [sub, ...rest] = args;
  const parsed = parseArgs(rest);
  const launchd = flagTri(parsed, 'launchd');
  const systemd = flagTri(parsed, 'systemd');
  const wantsLaunchd = launchd === true || (launchd !== false && systemd !== true && process.platform === 'darwin');
  const wantsSystemd = !wantsLaunchd && (systemd === true || (systemd !== false && process.platform === 'linux'));
  const paths = ensureDeskHome(io.env);
  const home = paths.home;
  const main = cliMain();

  if (sub === 'install') {
    if (wantsLaunchd) {
      const target = path.join(homedir(), 'Library', 'LaunchAgents', 'com.opencovenant.desk.plist');
      mkdirSync(path.dirname(target), { recursive: true });
      writeFileSync(
        target,
        readFileSync(path.join(serviceDir(), 'com.opencovenant.desk.plist'), 'utf8')
          .replaceAll('__NODE__', process.execPath)
          .replaceAll('__DESK_MAIN__', main)
          .replaceAll('__DESK_HOME__', home),
      );
      io.out(`Wrote ${target}.`);
      io.out(`Load it with: launchctl bootstrap gui/$(id -u) ${target}`);
      return 0;
    }
    if (wantsSystemd) {
      const target = path.join(homedir(), '.config', 'systemd', 'user', 'covenant-desk.service');
      mkdirSync(path.dirname(target), { recursive: true });
      writeFileSync(
        target,
        readFileSync(path.join(serviceDir(), 'covenant-desk.service'), 'utf8')
          .replaceAll('__DESK_MAIN__', `${process.execPath} ${main}`)
          .replaceAll('__DESK_HOME__', home),
      );
      io.out(`Wrote ${target}.`);
      io.out('Enable it with: systemctl --user daemon-reload && systemctl --user enable --now covenant-desk');
      return 0;
    }
    return usage(io, 'Say which one to install: covenant-desk service install --launchd or --systemd');
  }

  if (sub === 'uninstall') {
    const targets = [
      path.join(homedir(), 'Library', 'LaunchAgents', 'com.opencovenant.desk.plist'),
      path.join(homedir(), '.config', 'systemd', 'user', 'covenant-desk.service'),
    ].filter((file) => existsSync(file));
    if (targets.length === 0) {
      io.out('No service file is installed.');
      return 0;
    }
    for (const target of targets) {
      rmSync(target, { force: true });
      io.out(`Removed ${target}.`);
    }
    io.out('Unload it with: launchctl bootout gui/$(id -u)/com.opencovenant.desk');
    io.out('On Linux: systemctl --user disable --now covenant-desk && systemctl --user daemon-reload');
    return 0;
  }

  return usage(io, 'Use: covenant-desk service install --launchd|--systemd, or service uninstall');
};

export const HANDLERS: Record<string, CommandHandler> = {
  init,
  start,
  stop,
  status,
  quote,
  premium,
  pools,
  order,
  hedge,
  record,
  mcp,
  service,
};

/**
 * Run one command. Returns the process exit code:
 * 0 success, 1 a command failed, 2 the command line was wrong.
 */
export async function runCli(io: CliIo): Promise<number> {
  const args = [...io.argv];
  const command = args.shift();

  if (!command || command === '--help' || command === '-h' || command === 'help') {
    io.out(helpText());
    return command ? 0 : 2;
  }
  if (command === '--version' || command === '-v') {
    io.out(version());
    return 0;
  }

  const handler = HANDLERS[command];
  if (!handler) {
    io.err(`Unknown command "${command}". Run "covenant-desk --help" for the list.`);
    return 2;
  }

  try {
    return await handler(args, io);
  } catch (error) {
    const { error: code, reason } = toErrorResponse(error);
    io.err(`${code}: ${reason}`);
    return 1;
  }
}
