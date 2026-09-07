import { afterEach, describe, expect, it } from 'vitest';
import { mkdtempSync, readFileSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { defaultConfig } from '../../src/core/config.js';
import { silentLogger } from '../../src/core/logger.js';
import { openStore, type Store } from '../../src/core/store.js';
import type { Address, Pool } from '../../src/core/types.js';
import { createSession } from '../../src/fairvalue/session.js';
import { createReference } from '../../src/fairvalue/reference.js';
import { createPremium } from '../../src/fairvalue/premium.js';
import { createPaired } from '../../src/fairvalue/paired.js';
import { createRecorder } from '../../src/fairvalue/recorder.js';
import { AAPL, AI, NVDA, USDG, lighterSource, makeChain, makePool, makeToken, poolsFor, rhjSource } from './helpers.js';

/** Saturday 2026-09-19, 08:00 New York time. */
const WEEKEND = Date.UTC(2026, 8, 19, 12);

const session = createSession();
const stores: Store[] = [];
const directories: string[] = [];

afterEach(() => {
  while (stores.length > 0) stores.pop()?.close();
  while (directories.length > 0) rmSync(directories.pop() as string, { recursive: true, force: true });
});

const stock = {
  NVDA: makeToken({ address: NVDA, symbol: 'NVDA', uiMultiplier: 1 }),
  AAPL: makeToken({ address: AAPL, symbol: 'AAPL', uiMultiplier: 1 }),
};

const nvdaUsdg = makePool({ poolId: '0xnvdausdg', currency0: NVDA, currency1: USDG, mid: 190, liquidity: 50_000n });
const aiNvda = makePool({ poolId: '0xainvda', currency0: AI, currency1: NVDA, mid: 0.001, liquidity: 3_000n });
const aiAapl = makePool({ poolId: '0xaiaapl', currency0: AI, currency1: AAPL, mid: 0.0006, liquidity: 1_000n });

function build(pools: readonly Pool[], marks: Record<string, number> = { NVDA: 180 }) {
  const store = openStore({ path: ':memory:' });
  stores.push(store);
  store.upsertToken(makeToken({ address: AI, symbol: 'AI', isStockToken: false }));
  store.upsertToken(stock.NVDA);
  store.upsertToken(stock.AAPL);
  for (const pool of pools) store.upsertPool(pool);

  const byAddress = new Map([
    [NVDA.toLowerCase(), stock.NVDA],
    [AAPL.toLowerCase(), stock.AAPL],
  ]);

  const chain = makeChain({
    registry: {
      token: (key) =>
        byAddress.get(key.toLowerCase()) ??
        (key.toUpperCase() === 'NVDA' ? stock.NVDA : key.toUpperCase() === 'AAPL' ? stock.AAPL : undefined),
      stockTokens: () => [stock.NVDA, stock.AAPL],
      isStockToken: (address) => byAddress.has(address.toLowerCase()),
      feedFor: () => undefined,
    },
    pools: { forToken: poolsFor(pools) },
  });

  const reference = createReference({
    logger: silentLogger(),
    chain,
    session,
    rhj: rhjSource({}),
    lighter: lighterSource(
      Object.fromEntries(
        Object.entries(marks).map(([symbol, markPrice]) => [
          symbol,
          { symbol, marketId: 1, markPrice, asOf: WEEKEND - 5_000 },
        ]),
      ),
    ),
    now: () => WEEKEND,
  });

  const config = defaultConfig({ recorderIntervalSec: 3600 });
  const premium = createPremium({ config, logger: silentLogger(), store, chain, session, reference, now: () => WEEKEND });
  const paired = createPaired({ config, logger: silentLogger(), store, chain, premium, now: () => WEEKEND });
  const recorder = createRecorder({
    config,
    logger: silentLogger(),
    store,
    chain,
    session,
    premium,
    paired,
    now: () => WEEKEND,
  });

  return { recorder, store, premium };
}

describe('the recorder', () => {
  it('writes one row per stock token and one per paired pool', async () => {
    const { recorder, store } = build([nvdaUsdg, aiNvda]);
    expect(await recorder.tick()).toBe(2);

    const rows = store.listObservations();
    const nvda = rows.find((row) => row.symbol === 'NVDA');
    expect(nvda).toMatchObject({
      token: NVDA,
      pool: '0xnvdausdg',
      referenceSource: 'lighter-rh',
      sessionState: 'closed',
    });
    expect(nvda?.onchainMidUsd).toBeCloseTo(190, 9);
    expect(nvda?.referenceUsd).toBeCloseTo(180, 9);
    expect(nvda?.premiumBps).toBeCloseTo((190 / 180 - 1) * 10_000, 6);
    expect(nvda?.candidates?.length).toBe(1);

    const ai = rows.find((row) => row.symbol === 'AI');
    expect(ai).toMatchObject({ stockSymbol: 'NVDA', pool: '0xainvda', referenceSource: 'lighter-rh' });
    expect(ai?.onchainMidUsd).toBeCloseTo(0.19, 9);
    expect(ai?.referenceUsd).toBeCloseTo(0.18, 9);
    expect(ai?.liquidity).toBe(3_000n);
  });

  it('names the reference source on a paired row whose stock leg it did not write', async () => {
    const { recorder, store, premium } = build([nvdaUsdg, aiNvda]);
    premium.all = async () => [];

    expect(await recorder.tick()).toBe(1);
    const [ai] = store.listObservations();
    expect(ai?.symbol).toBe('AI');
    expect(ai?.referenceSource).toBe('lighter-rh');
  });

  it('keeps going when one pool cannot be priced', async () => {
    // AAPL has no reference, so the AI/AAPL pool has to be skipped.
    const { recorder, store } = build([nvdaUsdg, aiNvda, aiAapl]);
    expect(await recorder.tick()).toBe(2);
    expect(store.listObservations().map((row) => row.symbol).sort()).toEqual(['AI', 'NVDA']);
  });

  it('records nothing and does not throw when the chain is unreachable', async () => {
    const { recorder, store } = build([]);
    expect(await recorder.tick()).toBe(0);
    expect(store.listObservations()).toHaveLength(0);
  });

  it('starts and stops without leaving a timer behind', async () => {
    const { recorder } = build([nvdaUsdg, aiNvda]);
    expect(recorder.running()).toBe(false);
    recorder.start();
    expect(recorder.running()).toBe(true);
    recorder.start();
    recorder.stop();
    expect(recorder.running()).toBe(false);
  });

  it('exports the rows as CSV', async () => {
    const { recorder } = build([nvdaUsdg, aiNvda]);
    await recorder.tick();

    const directory = mkdtempSync(path.join(tmpdir(), 'desk-recorder-'));
    directories.push(directory);
    const file = await recorder.export(path.join(directory, 'nested', 'premium.csv'));

    const lines = readFileSync(file, 'utf8').trim().split('\n');
    expect(lines[0]).toBe(
      'ts,iso,symbol,token,stock_symbol,pool,onchain_mid_usd,reference_usd,reference_source,premium_bps,session_state,block_number,liquidity',
    );
    expect(lines).toHaveLength(3);
    expect(lines.some((line) => line.includes('NVDA,') && line.includes('lighter-rh'))).toBe(true);
  });

  it('filters an export by symbol', async () => {
    const { recorder } = build([nvdaUsdg, aiNvda]);
    await recorder.tick();

    const directory = mkdtempSync(path.join(tmpdir(), 'desk-recorder-'));
    directories.push(directory);
    const file = await recorder.export(path.join(directory, 'ai.csv'), { symbol: 'AI' });

    const lines = readFileSync(file, 'utf8').trim().split('\n');
    expect(lines).toHaveLength(2);
    expect(lines[1]?.split(',')[2]).toBe('AI');
  });
});

describe('paired pool selection', () => {
  it('leaves stablecoin pools out of the paired set', async () => {
    const { recorder, store } = build([nvdaUsdg, aiNvda]);
    await recorder.tick();

    const paired = store.listObservations().filter((row) => row.stockSymbol !== undefined);
    expect(paired.map((row) => row.token)).toEqual([AI.toLowerCase() as Address]);
  });
});
