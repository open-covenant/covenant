/**
 * The viem client every chain read goes through.
 *
 * Two things are added on top of a plain public client. Requests are spaced so
 * the public Robinhood Chain endpoint does not answer with "too many requests"
 * under a pool scan, and a request that is refused for that reason is retried
 * with a widening pause instead of failing the caller. Batched reads go through
 * Multicall3 in fixed-size groups so one slow group cannot stall a whole
 * refresh.
 */

import {
  createPublicClient,
  createWalletClient,
  defineChain,
  http,
  type Account,
  type Chain,
  type PublicClient,
  type Transport,
  type WalletClient,
} from 'viem';
import { privateKeyToAccount } from 'viem/accounts';
import type { Config } from '../core/config.js';
import type { Logger } from '../core/logger.js';
import { UpstreamError } from '../core/errors.js';
import type { Address } from '../core/types.js';
import { isRateLimitError } from './logs.js';

/** Multicall3, deployed at the same address here as everywhere else. */
export const MULTICALL3_ADDRESS: Address = '0xcA11bde05977b3631167028862bE2a173976CA11';

/** Robinhood Chain, an Arbitrum Nitro Orbit chain settling in ETH. */
export const robinhoodChain: Chain = defineChain({
  id: 4663,
  name: 'Robinhood Chain',
  nativeCurrency: { name: 'Ether', symbol: 'ETH', decimals: 18 },
  rpcUrls: { default: { http: ['https://rpc.mainnet.chain.robinhood.com'] } },
  blockExplorers: { default: { name: 'Blockscout', url: 'https://robinhoodchain.blockscout.com' } },
  contracts: { multicall3: { address: MULTICALL3_ADDRESS, blockCreated: 0 } },
});

/** One `eth_call` through Multicall3. */
export interface ContractCall {
  readonly address: Address;
  readonly abi: readonly unknown[];
  readonly functionName: string;
  readonly args?: readonly unknown[];
}

/** A call that succeeded, or the reason it did not. */
export type CallOutcome<T> =
  | { readonly status: 'success'; readonly result: T }
  | { readonly status: 'failure' };

export interface ChainClientOptions {
  /** Smallest gap between two requests, milliseconds. */
  readonly minRequestIntervalMs?: number;
  /** Attempts made when the endpoint answers "too many requests". */
  readonly rateLimitRetries?: number;
  /** Calls per Multicall3 round trip. */
  readonly multicallBatchSize?: number;
  /** Request timeout, milliseconds. */
  readonly timeoutMs?: number;
}

const DEFAULTS = {
  minRequestIntervalMs: 90,
  rateLimitRetries: 6,
  multicallBatchSize: 200,
  timeoutMs: 120_000,
} as const;

/** Public reads, batched reads, and the signer when one is configured. */
export interface DeskChainClient {
  readonly chainId: number;
  blockNumber(): Promise<bigint>;
  multicall<T>(calls: readonly unknown[]): Promise<T[]>;
  /** Batched reads that report per-call failures instead of throwing. */
  multicallAllowFailure<T>(calls: readonly ContractCall[]): Promise<CallOutcome<T>[]>;
  /** A raw JSON-RPC request, spaced and retried like every other call. */
  request<T>(method: string, params: readonly unknown[]): Promise<T>;
  readonly publicClient: PublicClient<Transport, Chain>;
  /** The address the desk would sign from, when a key is loaded. */
  account(): Account | undefined;
  /** A wallet client bound to the desk key. Undefined when no key is loaded. */
  walletClient(): WalletClient<Transport, Chain, Account> | undefined;
}

/** Build the client for a config. `privateKey` is optional and never logged. */
export function createChainClient(
  config: Config,
  logger: Logger,
  privateKey?: string,
  options: ChainClientOptions = {},
): DeskChainClient {
  const minInterval = options.minRequestIntervalMs ?? DEFAULTS.minRequestIntervalMs;
  const retries = options.rateLimitRetries ?? DEFAULTS.rateLimitRetries;
  const batchSize = options.multicallBatchSize ?? DEFAULTS.multicallBatchSize;
  const timeout = options.timeoutMs ?? DEFAULTS.timeoutMs;

  const gate = createRequestGate(minInterval, retries, logger);
  const chain: Chain = { ...robinhoodChain, id: config.chainId };
  const transport = throttledHttp(config.rpcUrl, timeout, gate);

  const publicClient = createPublicClient({ chain, transport }) as PublicClient<Transport, Chain>;

  const account = privateKey ? privateKeyToAccount(normalizeKey(privateKey)) : undefined;
  const wallet = account
    ? (createWalletClient({ account, chain, transport }) as WalletClient<Transport, Chain, Account>)
    : undefined;

  const runMulticall = async (
    calls: readonly ContractCall[],
    allowFailure: boolean,
  ): Promise<unknown[]> => {
    const out: unknown[] = [];
    for (let index = 0; index < calls.length; index += batchSize) {
      const slice = calls.slice(index, index + batchSize);
      const results = await publicClient.multicall({
        // viem's contract types are stricter than the desk's runtime shape; the
        // ABIs are validated by the calls that use them.
        contracts: slice as never,
        allowFailure: allowFailure as never,
        batchSize: 0,
      });
      out.push(...(results as unknown[]));
    }
    return out;
  };

  return {
    chainId: config.chainId,
    publicClient,
    account: () => account,
    walletClient: () => wallet,

    async blockNumber() {
      try {
        return await publicClient.getBlockNumber({ cacheTime: 0 });
      } catch (error) {
        throw new UpstreamError('rpc', `eth_blockNumber failed: ${messageOf(error)}`, {
          rpcUrl: config.rpcUrl,
        });
      }
    },

    async multicall<T>(calls: readonly unknown[]): Promise<T[]> {
      const results = await runMulticall(calls as readonly ContractCall[], false);
      return results as T[];
    },

    async multicallAllowFailure<T>(calls: readonly ContractCall[]): Promise<CallOutcome<T>[]> {
      const results = (await runMulticall(calls, true)) as { status: string; result?: unknown }[];
      return results.map((entry) =>
        entry.status === 'success'
          ? ({ status: 'success', result: entry.result as T } as const)
          : ({ status: 'failure' } as const),
      );
    },

    async request<T>(method: string, params: readonly unknown[]): Promise<T> {
      return (await publicClient.request({ method, params } as never)) as T;
    },
  };
}

/** A private key with the `0x` prefix viem expects. */
function normalizeKey(key: string): `0x${string}` {
  const trimmed = key.trim();
  return (trimmed.startsWith('0x') ? trimmed : `0x${trimmed}`) as `0x${string}`;
}

function messageOf(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

/** Serializes requests and answers a rate-limit refusal by waiting. */
type RequestGate = <T>(run: () => Promise<T>) => Promise<T>;

/** Widest spacing the gate will impose on itself, milliseconds. */
const MAX_REQUEST_INTERVAL_MS = 2_000;
/** Longest single pause after the endpoint refuses for rate, milliseconds. */
const MAX_RATE_LIMIT_BACKOFF_MS = 30_000;
/** Requests that must succeed in a row before the gate speeds back up. */
const SPEEDUP_AFTER = 25;
/** How long every request holds off after the endpoint stops answering, milliseconds. */
const COOLDOWN_MS = 60_000;

/**
 * Space requests, and slow down when the endpoint says to.
 *
 * The public endpoint answers a burst with "too many requests" rather than a
 * queue, and a pool scan is a burst by nature. The gate waits out each refusal
 * with a widening pause and also widens the gap it leaves between requests, so
 * a scan settles at a rate the endpoint will serve instead of hammering it. The
 * gap narrows again once requests are being answered.
 */
function createRequestGate(minIntervalMs: number, maxRetries: number, logger: Logger): RequestGate {
  let queue: Promise<unknown> = Promise.resolve();
  let lastStart = 0;
  let interval = minIntervalMs;
  let streak = 0;
  let cooldownUntil = 0;

  const attempt = async <T>(run: () => Promise<T>): Promise<T> => {
    for (let tries = 0; ; tries += 1) {
      const cooldown = cooldownUntil - Date.now();
      if (cooldown > 0) await sleep(cooldown);
      const wait = interval - (Date.now() - lastStart);
      if (wait > 0) await sleep(wait);
      lastStart = Date.now();
      try {
        const answer = await run();
        streak += 1;
        if (streak >= SPEEDUP_AFTER && interval > minIntervalMs) {
          interval = Math.max(minIntervalMs, Math.round(interval * 0.8));
          streak = 0;
        }
        return answer;
      } catch (error) {
        if (!isRateLimitError(error)) throw error;
        streak = 0;
        interval = Math.min(MAX_REQUEST_INTERVAL_MS, Math.round(interval * 1.5) + 50);
        if (tries >= maxRetries) {
          // The endpoint has stopped answering rather than queueing. Every
          // other loop holds off too, so the desk stops adding to the problem.
          cooldownUntil = Date.now() + COOLDOWN_MS;
          logger.warn('the endpoint is refusing requests, holding off', { forMs: COOLDOWN_MS });
          throw error;
        }
        const backoff = Math.min(MAX_RATE_LIMIT_BACKOFF_MS, 500 * 2 ** tries);
        logger.debug('rpc endpoint is rate limiting, waiting', {
          waitMs: backoff,
          spacingMs: interval,
          attempt: tries + 1,
        });
        await sleep(backoff);
      }
    }
  };

  return <T>(run: () => Promise<T>): Promise<T> => {
    const result = queue.then(() => attempt(run));
    queue = result.then(
      () => undefined,
      () => undefined,
    );
    return result;
  };
}

function sleep(ms: number): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

/** The stock http transport with every request routed through the gate. */
function throttledHttp(url: string, timeout: number, gate: RequestGate): Transport {
  const inner = http(url, { timeout, retryCount: 0, batch: false });
  return ((parameters) => {
    const created = inner(parameters);
    const original = created.request;
    return {
      ...created,
      request: ((args: unknown) => gate(() => original(args as never))) as typeof created.request,
    };
  }) as Transport;
}
