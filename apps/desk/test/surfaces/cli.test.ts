import { mkdtempSync, readFileSync, rmSync, statSync } from 'node:fs';
import { createServer } from 'node:net';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { afterEach, beforeEach, describe, expect, it } from 'vitest';
import { saveConfig } from '../../src/core/config.js';
import {
  COMMANDS,
  HANDLERS,
  RESTRICTIONS_NOTICE,
  displaySymbol,
  helpText,
  parseArgs,
  runCli,
  version,
  type CliIo,
} from '../../src/surfaces/cli/index.js';
import { deskFixture, startDesk, type RunningDesk } from './fixtures.js';

let home: string;

beforeEach(() => {
  home = mkdtempSync(path.join(tmpdir(), 'desk-cli-'));
});

afterEach(() => {
  rmSync(home, { recursive: true, force: true });
});

function cli(argv: string[], env: NodeJS.ProcessEnv = { DESK_HOME: home }) {
  const out: string[] = [];
  const err: string[] = [];
  const io: CliIo = { argv, env, out: (text) => out.push(text), err: (text) => err.push(text) };
  return { out, err, io, run: () => runCli(io) };
}

/** A port that was free a moment ago, so a connection to it is refused. */
async function freePort(): Promise<number> {
  const server = createServer();
  await new Promise<void>((resolve) => server.listen(0, '127.0.0.1', resolve));
  const address = server.address();
  const port = typeof address === 'object' && address ? address.port : 0;
  await new Promise<void>((resolve) => server.close(() => resolve()));
  return port;
}

describe('symbols in tables', () => {
  it('cuts a long symbol and drops anything unprintable', () => {
    expect(displaySymbol('NVDA')).toBe('NVDA');
    expect(displaySymbol('RealWorldAssetsRWAOfTheCentury')).toHaveLength(12);
    expect(displaySymbol('AI\u001b[31m')).toBe('AI [31m'.slice(0, 12));
    expect(displaySymbol('', '0xabc...')).toBe('0xabc...');
  });
});

describe('per-command help', () => {
  it('prints the flags of the command that was asked about', async () => {
    const { out, run } = cli(['order', '--help']);
    expect(await run()).toBe(0);
    const text = out.join('\n');
    expect(text).toContain('--amount-tokens');
    expect(text).toContain('--price-lte');
    expect(text).toContain('--legs');
  });

  it('answers help for every command it lists', async () => {
    for (const command of COMMANDS) {
      const { out, run } = cli([command.name, '--help']);
      expect(await run()).toBe(0);
      expect(out.join('\n')).toContain(`covenant-desk ${command.usage}`);
    }
  });
});

describe('parseArgs', () => {
  it('reads a value after a flag', () => {
    expect(parseArgs(['quote', 'NVDA', '--usd', '100'])).toEqual({
      positionals: ['quote', 'NVDA'],
      flags: { usd: '100' },
    });
  });

  it('reads a value joined with an equals sign', () => {
    expect(parseArgs(['--usd=250', '--side=sell']).flags).toEqual({ usd: '250', side: 'sell' });
  });

  it('treats a flag with no value as true and --no-flag as false', () => {
    expect(parseArgs(['--live', '--no-json']).flags).toEqual({ live: true, json: false });
  });

  it('treats a flag followed by another flag as true', () => {
    expect(parseArgs(['--foreground', '--json']).flags).toEqual({ foreground: true, json: true });
  });

  it('leaves a positional alone when a boolean flag comes first', () => {
    expect(parseArgs(['--json', 'NVDA'])).toEqual({ positionals: ['NVDA'], flags: { json: true } });
    expect(parseArgs(['--live', 'ord_1'])).toEqual({ positionals: ['ord_1'], flags: { live: true } });
    expect(parseArgs(['--include-traps', 'NVDA']).positionals).toEqual(['NVDA']);
  });

  it('stops reading options after a double dash', () => {
    expect(parseArgs(['export', '--', '--out'])).toEqual({ positionals: ['export', '--out'], flags: {} });
  });

  it('keeps negative numbers as values', () => {
    expect(parseArgs(['--price-lte', '-5']).flags).toEqual({ 'price-lte': '-5' });
  });
});

describe('help', () => {
  it('lists every command with what it does', () => {
    const text = helpText();
    for (const command of COMMANDS) expect(text).toContain(command.usage);
    expect(text).toContain('Keys stay on this machine.');
  });

  it('reads as product copy', () => {
    const copy = [helpText(), RESTRICTIONS_NOTICE, ...COMMANDS.map((command) => command.summary)].join('\n');
    expect(copy).not.toMatch(/—/);
    expect(copy).not.toMatch(/\b(canary|rollout|feature flag|shadow mode|promotion gate|backfill)\b/i);
    expect(copy).not.toMatch(/\b(institutional-grade|enterprise-grade|risk-free|guaranteed|production-ready)\b/i);
  });

  it('has a handler registered for every advertised command', () => {
    for (const command of COMMANDS) expect(HANDLERS[command.name]).toBeTypeOf('function');
  });
});

describe('dispatch', () => {
  it('prints help and exits 2 with no arguments', async () => {
    const { out, run } = cli([]);
    expect(await run()).toBe(2);
    expect(out.join('\n')).toContain('Usage: covenant-desk');
  });

  it('prints help and exits 0 when it was asked for', async () => {
    expect(await cli(['--help']).run()).toBe(0);
  });

  it('prints the package version', async () => {
    const { out, run } = cli(['--version']);
    expect(await run()).toBe(0);
    expect(out[0]).toBe(version());
    expect(out[0]).toMatch(/^\d+\.\d+\.\d+$/);
  });

  it('names an unknown command and points at help', async () => {
    const { err, run } = cli(['frobnicate']);
    expect(await run()).toBe(2);
    expect(err[0]).toContain('covenant-desk --help');
  });
});

describe('usage problems exit 2', () => {
  const cases: [string[], string][] = [
    [['quote'], 'covenant-desk quote NVDA'],
    [['pools'], 'covenant-desk pools --stock NVDA'],
    [['order', 'frobnicate'], 'order create|list|show|cancel'],
    [['order', 'create', '--side', 'buy'], 'both sides as addresses'],
    [['hedge', 'frobnicate'], 'hedge plan|apply|status|unwind'],
    [['hedge', 'plan'], 'covenant-desk hedge plan NVDA'],
    [['record'], 'record start|stop|export'],
    [['record', 'export'], '--out premium.csv'],
    [['service'], 'service install --launchd'],
    [['service', 'install', '--no-launchd', '--no-systemd'], 'which one to install'],
  ];

  for (const [argv, expected] of cases) {
    it(`covenant-desk ${argv.join(' ')}`, async () => {
      const { err, run } = cli(argv);
      expect(await run()).toBe(2);
      expect(err.join('\n')).toContain(expected);
    });
  }
});

describe('init', () => {
  it('writes the config, the key file, and the database, and prints the notice', async () => {
    const { out, run } = cli(['init']);
    expect(await run()).toBe(0);

    const text = out.join('\n');
    expect(text).toContain('Robinhood Assets (Jersey) Ltd');
    expect(text).toContain('covenant-desk init --acknowledge-restrictions');

    const config = JSON.parse(readFileSync(path.join(home, 'config.json'), 'utf8')) as Record<string, unknown>;
    expect(config.acknowledgedRestrictions).toBe(false);
    expect(config.live).toBe(false);
    expect(String(config.token)).toHaveLength(64);
    expect(statSync(path.join(home, 'config.json')).mode & 0o777).toBe(0o600);
    expect(statSync(path.join(home, 'keys.env')).mode & 0o777).toBe(0o600);
    expect(readFileSync(path.join(home, 'keys.env'), 'utf8')).toContain('DESK_EVM_PRIVATE_KEY=');
    expect(statSync(path.join(home, 'desk.sqlite')).isFile()).toBe(true);
  });

  it('keeps the token and records the acknowledgement on a second run', async () => {
    await cli(['init']).run();
    const first = JSON.parse(readFileSync(path.join(home, 'config.json'), 'utf8')) as { token: string };

    const { out, run } = cli(['init', '--acknowledge-restrictions']);
    expect(await run()).toBe(0);

    const second = JSON.parse(readFileSync(path.join(home, 'config.json'), 'utf8')) as {
      token: string;
      acknowledgedRestrictions: boolean;
    };
    expect(second.token).toBe(first.token);
    expect(second.acknowledgedRestrictions).toBe(true);
    expect(out.join('\n')).toContain('Restrictions acknowledged');
  });
});

describe('without a desk running', () => {
  it('says where it looked and how to start one', async () => {
    await cli(['init']).run();
    // A desk may well be running on the default port on this machine, so the
    // check points at a port that was just released.
    const port = String(await freePort());
    const { err, run } = cli(['status'], { DESK_HOME: home, DESK_PORT: port });
    expect(await run()).toBe(1);
    expect(err.join('\n')).toContain('No desk is answering on 127.0.0.1:');
    expect(err.join('\n')).toContain('covenant-desk start');
  });

  it('asks for init when there is no config file', async () => {
    const { err, run } = cli(['premium']);
    expect(await run()).toBe(1);
    expect(err.join('\n')).toContain('covenant-desk init');
  });
});

describe('against a running desk', () => {
  let running: RunningDesk | undefined;

  afterEach(async () => {
    await running?.close();
    running = undefined;
  });

  async function serve(desk = deskFixture()) {
    running = await startDesk(desk);
    const port = Number(new URL(running.url).port);
    saveConfig({ ...desk.config, port }, { DESK_HOME: home });
    return running;
  }

  it('prints the status table and the page link', async () => {
    await serve();
    const { out, run } = cli(['status']);
    expect(await run()).toBe(0);

    const text = out.join('\n');
    expect(text).toContain('Session');
    expect(text).toContain('closed');
    expect(text).toContain('dry run');
    expect(text).toContain('Page: http://127.0.0.1:');
  });

  it('prints the raw answer with --json', async () => {
    await serve();
    const { out, run } = cli(['status', '--json']);
    expect(await run()).toBe(0);
    expect(JSON.parse(out.join('\n'))).toMatchObject({ chainId: 4663 });
  });

  it('prints a premium table with a named reference source per row', async () => {
    const desk = deskFixture();
    desk.fairvalue.premium.all = async () => [
      {
        symbol: 'NVDA',
        token: '0xd0601CE157Db5bdC3162BbaC2a2C8aF5320D9EEC',
        onchainMid: { value: 182.5, unit: 'USD', source: 'pool', asOf: 1 },
        reference: { value: 180, unit: 'USD', source: 'chainlink', asOf: 1 },
        referenceSource: 'chainlink',
        candidates: [],
        premiumBps: { value: 138.9, unit: 'bps', source: 'derived', asOf: 1 },
        sessionState: 'open',
        asOf: 1,
      },
    ];
    await serve(desk);

    const { out, run } = cli(['premium', '--top', '5']);
    expect(await run()).toBe(0);
    const text = out.join('\n');
    expect(text).toContain('Symbol');
    expect(text).toContain('NVDA');
    expect(text).toContain('chainlink');
    expect(text).toContain('+139');
  });

  it('prices a token that trades below a cent without rounding it to zero', async () => {
    const desk = deskFixture();
    const AI = '0x7851b2d9d38cce3471168f115e567afaf27c1e18';
    const NVDA = '0xd0601ce157db5bdc3162bbac2a2c8af5320d9eec';
    desk.store.upsertToken({ address: AI, symbol: 'AI', name: 'Artificial Inu', decimals: 18, isStockToken: false });
    desk.fairvalue.paired.quote = async () => ({
      token: AI,
      symbol: 'AI',
      stockSymbol: 'NVDA',
      stockToken: NVDA,
      pool: `0x${'8'.repeat(64)}`,
      ratio: { value: 9.23484262585169e-8, unit: 'token', source: 'derived', asOf: 1 },
      usdOnchain: { value: 2.131984031955739e-5, unit: 'USD', source: 'derived', asOf: 1 },
      usdFair: { value: 2.1261992294532965e-5, unit: 'USD', source: 'derived', asOf: 1 },
      stockLegPremiumBps: { value: 27.2, unit: 'bps', source: 'derived', asOf: 1 },
      asOf: 1,
      bestEntryRoute: {
        pools: [`0x${'8'.repeat(64)}`],
        path: [AI, NVDA],
        usdPrice: { value: 2.131984031955739e-5, unit: 'USD', source: 'derived', asOf: 1 },
        note: 'Quoted in NVDA for 100 USD, price impact included.',
      },
      bestExitRoute: {
        pools: [`0x${'8'.repeat(64)}`],
        path: [AI, NVDA],
        usdPrice: { value: 2.131984031955739e-5, unit: 'USD', source: 'derived', asOf: 1 },
        note: 'Quoted in NVDA at the pool mid.',
      },
    });
    await serve(desk);

    const { out, run } = cli(['quote', 'AI', '--usd', '100']);
    expect(await run()).toBe(0);
    const text = out.join('\n');
    expect(text).toContain('0.00002132');
    expect(text).toContain('Quoted in NVDA for 100 USD, price impact included.');
    expect(text).toContain('+27');
    expect(text).not.toContain('Through ether');
  });

  it('shows a dry run fill against the order that produced it', async () => {
    const desk = deskFixture();
    desk.orders.book.get = (id) => ({
      id,
      kind: 'limit',
      side: 'buy',
      tokenIn: '0x5fc5360d0400a0fd4f2af552add042d716f1d168',
      tokenOut: '0xd0601ce157db5bdc3162bbac2a2c8af5320d9eec',
      amountIn: 50_000_000n,
      trigger: { priceLte: 500 },
      bounds: { maxSlippageBps: 100, maxOrderNotionalUsd: 250, maxBuyPremiumBps: 500 },
      live: false,
      status: 'filled',
      createdAt: 1,
      expiresAt: null,
      parentId: null,
      reason: 'Dry run recorded a fill of 49.83 USD at the quoted price.',
    });
    desk.store.insertExecution({
      id: 'exe_1',
      orderId: 'ord_1',
      live: false,
      amountIn: 50_000_000n,
      amountOut: 215_833_281_345_852_717n,
      quotedAmountOut: 215_833_281_345_852_717n,
      effectivePrice: { value: 0.004316665626917054, unit: 'token', source: 'pool', asOf: 1 },
      notionalUsd: { value: 49.828232082619174, unit: 'USD', source: 'derived', asOf: 1 },
      slippageBps: 0,
      status: 'simulated',
      reason: 'Dry run. No transaction was signed.',
      createdAt: 2,
    });
    await serve(desk);

    const { out, run } = cli(['order', 'show', 'ord_1']);
    expect(await run()).toBe(0);
    const text = out.join('\n');
    expect(text).toContain('filled');
    expect(text).toContain('dry run');
    expect(text).toContain('215833281345852717');
    expect(text).toContain('49.83');
    expect(text).toContain('Dry run. No transaction was signed.');
  });

  it('writes recorded observations to CSV', async () => {
    const desk = deskFixture();
    desk.store.insertObservation({
      ts: Date.now() - 60_000,
      symbol: 'NVDA',
      token: '0xd0601CE157Db5bdC3162BbaC2a2C8aF5320D9EEC',
      onchainMidUsd: 182.5,
      referenceUsd: 180,
      referenceSource: 'chainlink',
      premiumBps: 138.9,
      sessionState: 'open',
      blockNumber: 54_423_221n,
    });
    await serve(desk);

    const target = path.join(home, 'premium.csv');
    const { out, run } = cli(['record', 'export', '--out', target, '--symbol', 'NVDA']);
    expect(await run()).toBe(0);
    expect(out.join('\n')).toContain('Wrote 1 observations');

    const csv = readFileSync(target, 'utf8').trim().split('\n');
    expect(csv[0]).toBe(
      'ts,iso,symbol,token,pool,onchainMidUsd,referenceUsd,referenceSource,premiumBps,sessionState,blockNumber,stockSymbol,liquidity',
    );
    expect(csv[1]).toContain('NVDA');
    expect(csv[1]).toContain('chainlink');
    expect(csv[1]).toContain('138.9');
  });

  it('reports a refusal from the desk with the reason', async () => {
    await serve();
    const { err, run } = cli(['quote']);
    expect(await run()).toBe(2);
    expect(err.join('\n')).toContain('covenant-desk quote NVDA');
  });
});
