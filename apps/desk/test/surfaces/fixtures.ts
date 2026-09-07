/** A desk built from the real core and the module stubs, for surface tests. */

import { createChainModule } from '../../src/chain/index.js';
import { defaultConfig, type Config } from '../../src/core/config.js';
import { loadKeystore } from '../../src/core/keystore.js';
import { silentLogger } from '../../src/core/logger.js';
import { openStore } from '../../src/core/store.js';
import type { DeskStatus } from '../../src/core/types.js';
import { createFairValueModule } from '../../src/fairvalue/index.js';
import { createHedgeModule } from '../../src/hedge/index.js';
import { createOrdersModule } from '../../src/orders/index.js';
import type { Desk } from '../../src/daemon.js';
import { createHttpSurface, type HttpSurface } from '../../src/surfaces/http/index.js';

export const TEST_TOKEN = 'a'.repeat(64);

export function deskFixture(overrides: Partial<Config> = {}): Desk {
  const config = defaultConfig({ token: TEST_TOKEN, ...overrides });
  const logger = silentLogger();
  const store = openStore({ path: ':memory:' });
  const keystore = loadKeystore({ file: '/nonexistent/keys.env', env: {}, useKeychain: false });
  const chain = createChainModule({ config, logger, store });
  const fairvalue = createFairValueModule({ config, logger, store, chain });
  const orders = createOrdersModule({ config, logger, store, chain, fairvalue });
  const hedge = createHedgeModule({ config, logger, store, keystore, fairvalue });
  const startedAt = Date.now() - 42_000;

  return {
    config,
    logger,
    store,
    keystore,
    chain,
    fairvalue,
    orders,
    hedge,
    startedAt,
    start: async () => ({ url: `http://127.0.0.1:${config.port}` }),
    stop: async () => store.close(),
    scanPools: async () => 0,
    pricePools: async () => 0,
    nameTokens: async () => 0,
    status: async () =>
      ({
        version: '0.1.0',
        uptimeSec: 42,
        live: false,
        acknowledgedRestrictions: false,
        chainId: config.chainId,
        blockNumber: 54_423_221n,
        sessionState: 'closed',
        nextOpen: Date.now() + 3_600_000,
        tokensTracked: store.listTokens().length,
        poolsTracked: 0,
        poolScanRunning: false,
        openOrders: 0,
        observations24h: store.countObservationsSince(0),
        recorderRunning: false,
        hedgeEnabled: false,
        degraded: [],
        asOf: Date.now(),
      }) satisfies DeskStatus,
  };
}

export interface RunningDesk {
  readonly desk: Desk;
  readonly surface: HttpSurface;
  readonly url: string;
  call(path: string, init?: RequestInit & { token?: string | null }): Promise<{ status: number; body: any }>;
  close(): Promise<void>;
}

/** Start the API on a port the operating system picks. */
export async function startDesk(desk: Desk = deskFixture()): Promise<RunningDesk> {
  const surface = createHttpSurface(desk, { port: 0 });
  const { url } = await surface.start();

  return {
    desk,
    surface,
    url,
    async call(path, init = {}) {
      const { token, headers, ...rest } = init;
      const sent: Record<string, string> = { ...(headers as Record<string, string> | undefined) };
      if (token !== null) sent.authorization = `Bearer ${token ?? desk.config.token}`;
      const response = await fetch(`${url}${path}`, { ...rest, headers: sent });
      const text = await response.text();
      return { status: response.status, body: text.trim() === '' ? undefined : safeParse(text) };
    },
    async close() {
      await surface.stop();
      desk.store.close();
    },
  };
}

function safeParse(text: string): unknown {
  try {
    return JSON.parse(text);
  } catch {
    return text;
  }
}
