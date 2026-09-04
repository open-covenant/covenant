/**
 * Daemon wiring.
 *
 * The desk is built against a JSON-RPC endpoint served from this process, so
 * the pool scan, the cursor it records, and the status it reports are exercised
 * without touching a network.
 */

import { createServer, type Server } from 'node:http';
import { mkdtempSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { afterEach, describe, expect, it } from 'vitest';
import { defaultConfig } from '../src/core/config.js';
import { silentLogger } from '../src/core/logger.js';
import { loadKeystore } from '../src/core/keystore.js';
import { openStore, type Store } from '../src/core/store.js';
import { readPoolScanCursor } from '../src/chain/index.js';
import { createDesk, type Desk } from '../src/daemon.js';
import type { Address, Hex32, Pool } from '../src/core/types.js';

const TIP = 12_000_000n;

interface Rpc {
  readonly url: string;
  readonly calls: string[];
  close(): Promise<void>;
}

async function rpc(): Promise<Rpc> {
  const calls: string[] = [];
  const server: Server = createServer((req, res) => {
    const chunks: Buffer[] = [];
    req.on('data', (chunk: Buffer) => chunks.push(chunk));
    req.on('end', () => {
      const request = JSON.parse(Buffer.concat(chunks).toString('utf8')) as {
        id: number;
        method: string;
      };
      calls.push(request.method);
      const answer = (result: unknown) => {
        const text = JSON.stringify({ jsonrpc: '2.0', id: request.id, result });
        res.writeHead(200, { 'content-type': 'application/json' });
        res.end(text);
      };
      if (request.method === 'eth_blockNumber') return answer(`0x${TIP.toString(16)}`);
      if (request.method === 'eth_chainId') return answer('0x1237');
      if (request.method === 'eth_getLogs') return answer([]);
      return answer(null);
    });
  });

  await new Promise<void>((resolve) => server.listen(0, '127.0.0.1', () => resolve()));
  const address = server.address();
  const port = typeof address === 'object' && address ? address.port : 0;
  return {
    url: `http://127.0.0.1:${port}`,
    calls,
    close: () => new Promise<void>((resolve) => server.close(() => resolve())),
  };
}

const cleanup: (() => void)[] = [];

afterEach(() => {
  for (const close of cleanup.splice(0)) close();
});

async function fixture(): Promise<{ desk: Desk; store: Store; endpoint: Rpc }> {
  const endpoint = await rpc();
  const home = mkdtempSync(path.join(tmpdir(), 'desk-daemon-'));
  const store = openStore({ path: ':memory:' });
  const desk = createDesk({
    env: { DESK_HOME: home },
    config: defaultConfig({
      rpcUrl: endpoint.url,
      poolScanChunkBlocks: 5_000_000,
      recorderEnabled: false,
      hedgeEnabled: false,
    }),
    logger: silentLogger(),
    store,
    keystore: loadKeystore({ file: path.join(home, 'missing.env'), env: {}, useKeychain: false }),
  });

  cleanup.push(() => {
    store.close();
    void endpoint.close();
    rmSync(home, { recursive: true, force: true });
  });
  return { desk, store, endpoint };
}

/** How far each slice of the first pass reached, oldest first. */
function marks(store: Store): bigint[] {
  return store
    .listEvents({ kind: 'pool-scan', limit: 100 })
    .map((event) => BigInt(String((event.detail as { toBlock?: string }).toBlock)))
    .reverse();
}

describe('the pool scan', () => {
  it('covers the chain in slices and records how far each one reached', async () => {
    const { desk, store, endpoint } = await fixture();

    await desk.scanPools();

    expect(marks(store)).toEqual([4_999_999n, 9_999_999n, TIP]);
    expect(readPoolScanCursor(store)).toBe(TIP);
    expect(endpoint.calls.filter((method) => method === 'eth_getLogs').length).toBeGreaterThan(0);
  });

  it('starts from the mark the last pass left', async () => {
    const { desk, store, endpoint } = await fixture();

    await desk.scanPools();
    const first = endpoint.calls.filter((method) => method === 'eth_getLogs').length;
    await desk.scanPools();

    expect(endpoint.calls.filter((method) => method === 'eth_getLogs')).toHaveLength(first);
    expect(marks(store)).toHaveLength(3);
  });

  it('does not start a second pass while one is running', async () => {
    const { desk, store } = await fixture();

    const [, second] = await Promise.all([desk.scanPools(), desk.scanPools()]);

    expect(second).toBe(0);
    expect(marks(store)).toEqual([4_999_999n, 9_999_999n, TIP]);
  });
});

describe('pricing the pools the scan found', () => {
  it('reads the newest unpriced pools first and carries on from where it stopped', async () => {
    const { desk, store } = await fixture();
    const nvda = '0xd0601ce157db5bdc3162bbac2a2c8af5320d9eec' as Address;
    const other = '0x25e27b4824bcf9ef0e89fa99af184cfdbf265504' as Address;
    const pool = (poolId: string, block: bigint): Pool => ({
      poolId: poolId as Hex32,
      currency0: nvda,
      currency1: other,
      decimals0: 18,
      decimals1: 18,
      fee: 3000,
      tickSpacing: 60,
      hooks: '0x0000000000000000000000000000000000000000' as Address,
      initialBlock: block,
    });
    store.upsertPool(pool(`0x${'1'.repeat(64)}`, 100n));
    store.upsertPool(pool(`0x${'2'.repeat(64)}`, 200n));
    store.upsertPool(pool(`0x${'3'.repeat(64)}`, 300n));

    const asked: string[][] = [];
    desk.chain.pools.state = async (poolIds) => {
      asked.push([...poolIds]);
      return [];
    };

    expect(await desk.pricePools(2)).toBe(0);
    expect(await desk.pricePools(2)).toBe(0);
    expect(asked).toEqual([
      [`0x${'3'.repeat(64)}`, `0x${'2'.repeat(64)}`],
      [`0x${'1'.repeat(64)}`],
    ]);
  });

  it('starts again from the newest pool once nothing is left', async () => {
    const { desk } = await fixture();
    let calls = 0;
    desk.chain.pools.state = async () => {
      calls += 1;
      return [];
    };
    expect(await desk.pricePools()).toBe(0);
    expect(calls).toBe(0);
  });
});

describe('naming the tokens in new pools', () => {
  it('asks for the deepest tokens it has no name for, and skips the ones it knows', async () => {
    const { desk, store } = await fixture();
    const nvda = '0xd0601ce157db5bdc3162bbac2a2c8af5320d9eec' as Address;
    const deep = '0x7851b2d9d38cce3471168f115e567afaf27c1e18' as Address;
    const thin = '0x25e27b4824bcf9ef0e89fa99af184cfdbf265504' as Address;
    const unread = '0x48ed494633d2361872a1257c8cbdbad1331fd6e2' as Address;

    store.upsertToken({ address: nvda, symbol: 'NVDA', name: 'NVIDIA', decimals: 18, isStockToken: true });
    const pool = (poolId: string, other: Address, liquidity?: bigint): Pool => ({
      poolId: poolId as Hex32,
      currency0: nvda,
      currency1: other,
      decimals0: 18,
      decimals1: 18,
      fee: 3000,
      tickSpacing: 60,
      hooks: '0x0000000000000000000000000000000000000000' as Address,
      initialBlock: 1n,
      ...(liquidity === undefined ? {} : { liquidity }),
      trap: false,
    });
    store.upsertPool(pool(`0x${'1'.repeat(64)}`, deep, 1_000_000n));
    store.upsertPool(pool(`0x${'2'.repeat(64)}`, thin, 10n));
    store.upsertPool(pool(`0x${'3'.repeat(64)}`, unread));

    const asked: Address[][] = [];
    desk.chain.tokens.read = async (addresses) => {
      asked.push([...addresses]);
      return addresses.map((address) => ({
        address,
        symbol: address === deep ? 'AI' : 'MEME',
        name: address === deep ? 'Artificial Inu' : 'Meme',
        decimals: 18,
        isStockToken: false,
      }));
    };

    expect(await desk.nameTokens()).toBe(2);
    // Deepest first, the stock token it already knows left out, and a pool
    // whose liquidity has never been read left for a later pass.
    expect(asked).toEqual([[deep, thin]]);
  });

  it('has nothing to do when every token in a priced pool is named', async () => {
    const { desk } = await fixture();
    desk.chain.tokens.read = async () => {
      throw new Error('should not be called');
    };
    expect(await desk.nameTokens()).toBe(0);
  });
});

describe('status', () => {
  it('reports the block the scan has covered and the loops that are running', async () => {
    const { desk } = await fixture();
    await desk.scanPools();

    const status = await desk.status();

    expect(status.chainId).toBe(4663);
    expect(status.blockNumber).toBe(TIP);
    expect(status.poolScanRunning).toBe(false);
    expect(status.poolScanBlock).toBe(TIP);
    expect(status.recorderRunning).toBe(false);
    expect(status.live).toBe(false);
    expect(status.degraded).toEqual([]);
  });

  it('says the scan has not started before the first pass', async () => {
    const { desk } = await fixture();
    const status = await desk.status();
    expect(status.poolScanBlock).toBeUndefined();
  });
});
