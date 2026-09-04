/**
 * Live reads against Robinhood Chain mainnet. Run with `DESK_LIVE=1`.
 *
 * Every test here is read-only: `eth_call`, `eth_getLogs`, and two public HTTP
 * endpoints. Nothing is signed and no transaction is sent.
 */

import { describe, expect, it, beforeAll } from 'vitest';
import { defaultConfig } from '../../src/core/config.js';
import { createLogger, silentLogger } from '../../src/core/logger.js';
import { openStore, type Store } from '../../src/core/store.js';
import {
  CONTRACTS,
  createChainModule,
  USDG_DECIMALS,
  type ChainModule,
} from '../../src/chain/index.js';
import { erc20Abi } from '../../src/chain/abis.js';
import { sqrtPriceX96ToInversePrice } from '../../src/chain/price.js';
import type { Address, Hex32 } from '../../src/core/types.js';

const AAPL: Address = '0xaf3d76f1834a1d425780943c99ea8a608f8a93f9';
const NVDA: Address = '0xd0601ce157db5bdc3162bbac2a2c8af5320d9eec';
const USDG = CONTRACTS.usdg.toLowerCase() as Address;
/** The AAPL/USDG pool that prices at the oracle, from FACTS. */
const REFERENCE_POOL: Hex32 = '0xa2347ba69167e5602f74640ffbf737ee7cdd825e4726d3462564fc6533070147';

const verbose = process.env.DESK_LIVE_VERBOSE === '1';

let store: Store;
let chain: ChainModule;

beforeAll(() => {
  store = openStore({ path: ':memory:' });
  chain = createChainModule({
    config: defaultConfig(),
    logger: verbose ? createLogger({ level: 'debug' }) : silentLogger(),
    store,
  });
});

describe('registry', () => {
  it('loads every Robinhood stock token from the assets API', async () => {
    const counts = await chain.registry.refresh();
    expect(counts.tokens).toBe(194);
    expect(counts.feeds).toBe(52);
    expect(counts.markets).toBeGreaterThanOrEqual(50);

    const aapl = chain.registry.token('AAPL');
    expect(aapl?.address).toBe(AAPL);
    expect(aapl?.feed).toBeDefined();
    expect(aapl?.uiMultiplier).toBeGreaterThanOrEqual(1);
    expect(chain.registry.lighterPerpFor('NVDA')).toBe(15);
    expect(chain.registry.lighterPerpFor('AAPL')).toBe(10);
    console.log(
      `registry: ${counts.tokens} tokens, ${counts.feeds} feeds, ${counts.markets} Lighter markets, ` +
        `AAPL multiplier ${aapl?.uiMultiplier}`,
    );
  });
});

describe('token reads', () => {
  it('reads USDG decimals from the token, not from memory', async () => {
    const decimals = (await chain.client.publicClient.readContract({
      address: USDG,
      abi: erc20Abi,
      functionName: 'decimals',
    })) as number;
    expect(Number(decimals)).toBe(USDG_DECIMALS);
    console.log(`USDG decimals on chain: ${decimals}`);
  });

  it('reads the ERC-8056 fields of a stock token', async () => {
    const [aapl, nvda] = await chain.tokens.read([AAPL, NVDA]);
    expect(aapl?.symbol).toBe('AAPL');
    expect(aapl?.decimals).toBe(18);
    expect(aapl?.isStockToken).toBe(true);
    expect(aapl?.uiMultiplier).toBeGreaterThanOrEqual(1);
    expect(aapl?.tokenPaused).toBe(false);
    expect(nvda?.symbol).toBe('NVDA');
    console.log(
      `AAPL multiplier ${aapl?.uiMultiplier}, token paused ${aapl?.tokenPaused}, oracle paused ${aapl?.oraclePaused}`,
    );
  });
});

describe('Chainlink feeds', () => {
  it('reads the NVDA equity feed', async () => {
    const feed = chain.registry.feedFor('NVDA');
    expect(feed).toBeDefined();
    const round = await chain.feeds.latestRoundData(feed as Address);
    expect(round.decimals).toBe(8);
    expect(round.price.value).toBeGreaterThan(0);
    expect(round.price.source).toBe('chainlink');
    expect(round.updatedAt).toBeGreaterThan(0);
    console.log(
      `NVDA/USD ${round.price.value.toFixed(2)} published ${new Date(round.updatedAt).toISOString()} ` +
        `(${Math.round((Date.now() - round.updatedAt) / 1000)} s ago)`,
    );
  });

  it('reads ETH/USD and USDG/USD', async () => {
    const [eth, usdg] = await Promise.all([chain.feeds.ethUsd(), chain.feeds.usdgUsd()]);
    expect(eth.price.value).toBeGreaterThan(0);
    expect(usdg.price.value).toBeGreaterThan(0.9);
    expect(usdg.price.value).toBeLessThan(1.1);
    console.log(`ETH/USD ${eth.price.value.toFixed(2)}, USDG/USD ${usdg.price.value.toFixed(4)}`);
  });
});

describe('pool discovery', () => {
  it('finds the AAPL reference pool and at least 100 stock-quoted pools', async () => {
    const started = Date.now();
    const pools = await chain.pools.discover({
      currencies: [AAPL, NVDA],
      onProgress: verbose
        ? ({ range, found }) => console.log(`scanned to ${range.to}, ${found} pools`)
        : undefined,
    });
    console.log(
      `discovery: ${pools.length} pools holding AAPL or NVDA in ${Math.round((Date.now() - started) / 1000)} s`,
    );

    expect(pools.length).toBeGreaterThanOrEqual(100);
    const reference = pools.find((pool) => pool.poolId === REFERENCE_POOL);
    expect(reference).toBeDefined();
    expect(reference?.currency0).toBe(USDG);
    expect(reference?.currency1).toBe(AAPL);
    expect(reference?.decimals0).toBe(USDG_DECIMALS);
    expect(reference?.decimals1).toBe(18);
    expect(reference?.fee).toBe(0x800000);
    expect(reference?.tickSpacing).toBe(10);
    expect(reference?.hooks).toBe('0x70a9a88402989226847ec122043ce5e7ff462080');

    // Both currency positions are covered: some pools hold the stock token as
    // currency0, others as currency1.
    expect(pools.some((pool) => pool.currency0 === AAPL || pool.currency0 === NVDA)).toBe(true);
    expect(pools.some((pool) => pool.currency1 === AAPL || pool.currency1 === NVDA)).toBe(true);
  }, 300_000);

  it('reads state for the deepest AAPL pools and flags the traps', async () => {
    const ranked = await chain.pools.forToken(AAPL, { limit: 10, includeTraps: true });
    expect(ranked.length).toBeGreaterThan(0);
    for (const pool of ranked.slice(0, 10)) {
      const feeBps = (pool.lpFee ?? 0) / 100;
      console.log(
        `pool ${pool.poolId.slice(0, 12)} liquidity ${pool.liquidity} fee ${feeBps} bps trap ${pool.trap === true}`,
      );
    }
    const traps = ranked.filter((pool) => pool.trap === true);
    for (const trap of traps) expect((trap.lpFee ?? 0) / 100).toBeGreaterThan(300);
  }, 180_000);

  it('prices AAPL from the reference pool against Chainlink', async () => {
    const [pool] = await chain.pools.state([REFERENCE_POOL]);
    expect(pool?.sqrtPriceX96).toBeGreaterThan(0n);
    expect(pool?.trap).toBe(false);

    // currency0 is USDG, so the pool mid is AAPL per USDG. Invert it for the
    // price the chain is charging per AAPL.
    const usdgPerAapl = sqrtPriceX96ToInversePrice(
      pool?.sqrtPriceX96 as bigint,
      pool?.decimals0 as number,
      pool?.decimals1 as number,
    );
    expect(Number.isFinite(usdgPerAapl)).toBe(true);
    expect(usdgPerAapl).toBeGreaterThan(0);

    const feed = chain.registry.feedFor('AAPL') as Address;
    const round = await chain.feeds.latestRoundData(feed);
    const premiumBps = (usdgPerAapl / round.price.value - 1) * 10_000;
    const ageSec = Math.round((Date.now() - round.updatedAt) / 1000);

    console.log(
      `AAPL on chain ${usdgPerAapl.toFixed(4)} USDG, Chainlink ${round.price.value.toFixed(4)} USD ` +
        `(published ${ageSec} s ago), premium ${premiumBps.toFixed(1)} bps`,
    );

    // The equity feed updates on a 0.5% move while the United States market is
    // open and freezes outside it, so a recent publication means the session is
    // live and the two prices should agree.
    if (ageSec < 600) {
      expect(Math.abs(premiumBps)).toBeLessThan(100);
    } else {
      console.log(
        'the equity feed is frozen, so the premium above is the number the desk exists to report',
      );
    }
  });
});

describe('V4Quoter', () => {
  it('quotes 10 USDG into AAPL through the reference pool', async () => {
    const quote = await chain.quoter.quoteExactInputSingle({
      tokenIn: USDG,
      tokenOut: AAPL,
      amountIn: 10_000_000n,
      poolId: REFERENCE_POOL,
    });
    expect(quote.amountOut).toBeGreaterThan(0n);
    expect(quote.route).toEqual([REFERENCE_POOL]);
    expect(quote.gasEstimate).toBeGreaterThan(0n);
    const aaplOut = Number(quote.amountOut) / 1e18;
    console.log(
      `10 USDG buys ${aaplOut.toFixed(9)} AAPL, ${(1 / quote.effectivePrice.value).toFixed(4)} USDG per AAPL, ` +
        `gas estimate ${quote.gasEstimate}`,
    );
    expect(aaplOut).toBeGreaterThan(0.01);
    expect(aaplOut).toBeLessThan(1);
  });

  it('picks a route on its own when no pool is named', async () => {
    const quote = await chain.quoter.quoteExactInput({
      tokenIn: USDG,
      tokenOut: AAPL,
      amountIn: 10_000_000n,
    });
    expect(quote.amountOut).toBeGreaterThan(0n);
    expect(quote.route.length).toBeGreaterThanOrEqual(1);
    console.log(`chosen route ${quote.route.map((id) => id.slice(0, 12)).join(' -> ')}`);
  });
});

describe('swap encoding', () => {
  it('builds calldata for a swap it will not send', async () => {
    const encoded = await chain.swap.encode({
      tokenIn: USDG,
      tokenOut: AAPL,
      amountIn: 10_000_000n,
      minAmountOut: 1n,
      poolId: REFERENCE_POOL,
      deadlineSec: 120,
      live: false,
    });
    expect(encoded.to).toBe(CONTRACTS.universalRouter);
    expect(encoded.value).toBe(0n);
    expect(encoded.data.startsWith('0x3593564c')).toBe(true);

    // No key is loaded in this test, so execute stays a simulation and says so.
    const result = await chain.swap.execute({
      tokenIn: USDG,
      tokenOut: AAPL,
      amountIn: 10_000_000n,
      minAmountOut: 1n,
      poolId: REFERENCE_POOL,
      deadlineSec: 120,
      live: false,
    });
    expect(result.simulated).toBe(true);
    expect(result.txHash).toBeUndefined();
    expect(result.amountOut).toBeGreaterThan(0n);
    console.log(`dry run: ${result.amountOut} AAPL raw, held back because ${result.reason}`);
  });
});
