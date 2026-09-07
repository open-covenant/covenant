/**
 * Live reads against Robinhood Chain and the two off-chain price sources.
 *
 * Run with `DESK_LIVE=1`. Every call here is a read: no transaction is signed,
 * no order is placed, and nothing is written outside a temporary database.
 *
 * The AAPL/USDG reference pool is seeded from `FACTS.md` rather than found by
 * a log scan, so this file measures fair value and not pool discovery.
 */

import { afterAll, beforeAll, describe, expect, it } from 'vitest';
import { mkdtempSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { defaultConfig } from '../../src/core/config.js';
import { createLogger } from '../../src/core/logger.js';
import { openStore, type Store } from '../../src/core/store.js';
import { createChainModule, type ChainModule } from '../../src/chain/index.js';
import { createFairValueModule, type FairValueModule } from '../../src/fairvalue/index.js';
import { USDG_ADDRESS, otherCurrency, sameAddress } from '../../src/fairvalue/pools.js';
import type { Address, Hex32, Pool } from '../../src/core/types.js';

const AAPL = '0xaf3d76f1834a1d425780943c99ea8a608f8a93f9' as Address;

/** Prices at the oracle, dynamic fee, hook 0x70a9a884. Checked 2026-09-04. */
const REFERENCE_POOL: Pool = {
  poolId: '0xa2347ba69167e5602f74640ffbf737ee7cdd825e4726d3462564fc6533070147' as Hex32,
  currency0: USDG_ADDRESS,
  currency1: AAPL,
  decimals0: 6,
  decimals1: 18,
  fee: 0x800000,
  tickSpacing: 10,
  hooks: '0x70a9a88402989226847ec122043ce5e7ff462080' as Address,
  initialBlock: 0n,
};

let directory: string;
let store: Store;
let chain: ChainModule;
let fairvalue: FairValueModule;

beforeAll(async () => {
  directory = mkdtempSync(path.join(tmpdir(), 'desk-live-'));
  store = openStore({ path: path.join(directory, 'desk.sqlite') });
  const config = defaultConfig();
  const logger = createLogger({ level: 'error' });
  chain = createChainModule({ config, logger, store });
  fairvalue = createFairValueModule({ config, logger, store, chain });
  await chain.registry.refresh();
  store.upsertPool(REFERENCE_POOL);
});

afterAll(() => {
  store?.close();
  if (directory) rmSync(directory, { recursive: true, force: true });
});

describe('the calendar against the clock', () => {
  it('names a state and the next open', () => {
    const info = fairvalue.session.info(Date.now());
    expect(['open', 'extended', 'overnight', 'closed']).toContain(info.state);
    expect(info.nextOpen).toBeGreaterThan(Date.now() - 24 * 60 * 60 * 1000);
  });
});

describe('reference prices', () => {
  it('reads Chainlink, the perpetual mark, and the issuer ask for AAPL', async () => {
    const { chosen, candidates } = await fairvalue.reference.select('AAPL');

    expect(candidates.length).toBeGreaterThan(0);
    expect(chosen?.price.value).toBeGreaterThan(0);

    for (const candidate of candidates) {
      expect(candidate.price.value).toBeGreaterThan(1);
      expect(candidate.price.value).toBeLessThan(10_000);
    }

    // Every venue prices the same share, so they stay within a few percent.
    const prices = candidates.map((candidate) => candidate.price.value);
    expect(Math.max(...prices) / Math.min(...prices) - 1).toBeLessThan(0.1);
  });

  it('names the source it chose and why', async () => {
    const { chosen } = await fairvalue.reference.select('AAPL');

    expect(chosen?.source).toBeDefined();
    expect(chosen?.note).toBeTruthy();
    if (fairvalue.session.state(Date.now()) === 'closed') {
      expect(chosen?.source).not.toBe('chainlink');
    }
  });
});

describe('the on-chain price', () => {
  it('prices AAPL from the reference pool and reports the premium', async () => {
    const pools = await chain.pools.forToken(AAPL, { limit: 25 });
    const usdg = pools.filter((pool) => sameAddress(otherCurrency(pool, AAPL), USDG_ADDRESS));
    expect(usdg.length).toBeGreaterThan(0);

    const value = await fairvalue.premium.forSymbol('AAPL');
    expect(value.pool).toBeDefined();
    expect(value.reference?.value).toBeGreaterThan(0);
    expect(value.onchainMid?.value).toBeGreaterThan(0);

    // This pool prices at the oracle, so the gap stays inside a few percent.
    expect(Math.abs(value.premiumBps?.value ?? Number.POSITIVE_INFINITY)).toBeLessThan(500);
  });
});

describe('the recorder', () => {
  it('writes rows without stopping on a single failure', async () => {
    const before = Date.now() - 1_000;
    const written = await fairvalue.recorder.tick();
    expect(written).toBeGreaterThan(0);
    expect(store.countObservationsSince(before)).toBe(written);
  });
});
