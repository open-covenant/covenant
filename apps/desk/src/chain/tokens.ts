/**
 * Token reads.
 *
 * Every batch goes through Multicall3 in one round trip. A Robinhood stock
 * token adds four ERC-8056 members to the ERC-20 set: the share multiplier, the
 * multiplier scheduled to replace it, and the two issuer pause switches. Any
 * other ERC-20 answers only the ERC-20 half, so per-call failures are expected
 * and are treated as "this token does not have that member" rather than as an
 * error.
 */

import type { Logger } from '../core/logger.js';
import type { Store } from '../core/store.js';
import { NotFoundError } from '../core/errors.js';
import { rawQuantity, type Address, type Quantity, type Token } from '../core/types.js';
import { erc20Abi, stockTokenAbi } from './abis.js';
import type { CallOutcome, ContractCall, DeskChainClient } from './client.js';
import type { DeskRegistry } from './registry.js';

/** Decimals the ERC-8056 multiplier is published with. */
export const MULTIPLIER_DECIMALS = 18;

/** ERC-20 default, used only where a balance has to be shown for a token that did not answer. */
const FALLBACK_DECIMALS = 18;

/** Batched ERC-20 and ERC-8056 reads. */
export interface DeskTokenReader {
  /** Decimals, multiplier, and pause state for a batch of tokens, in one round trip. */
  read(addresses: readonly Address[]): Promise<Token[]>;
  balanceOf(token: Address, holder: Address): Promise<Quantity>;
  /** `balanceOfUI(address)`, the holding after the share multiplier is applied. */
  balanceOfUi(token: Address, holder: Address): Promise<Quantity>;
  /**
   * Decimals for a batch of tokens, keyed by lowercase address. A token that
   * did not answer is left out of the map rather than assumed, and is asked
   * again on the next call. Answers are cached for the life of the process.
   */
  decimalsOf(addresses: readonly Address[]): Promise<Map<string, number>>;
}

export interface TokenReaderDeps {
  readonly client: DeskChainClient;
  readonly logger: Logger;
  readonly store: Store;
  readonly registry: DeskRegistry;
}

/** Fields read per token, in the order the multicall returns them. */
const FIELDS = [
  'decimals',
  'symbol',
  'name',
  'uiMultiplier',
  'newUIMultiplier',
  'tokenPaused',
  'oraclePaused',
] as const;

/**
 * Decimals the desk does not need to ask for. USDG carries six, which is the
 * one that matters: a pool quoted in USDG is off by a factor of a trillion if
 * this is guessed at eighteen. Read from the token on chain on 2026-09-04.
 */
const KNOWN_DECIMALS: Record<string, number> = {
  '0x5fc5360d0400a0fd4f2af552add042d716f1d168': 6,
  '0x0bd7d308f8e1639fab988df18a8011f41eacad73': 18,
  '0x0000000000000000000000000000000000000000': 18,
};

export function createTokenReader(deps: TokenReaderDeps): DeskTokenReader {
  const logger = deps.logger.child({ component: 'chain.tokens' });
  const decimalsCache = new Map<string, number>(Object.entries(KNOWN_DECIMALS));

  /**
   * Remember a token's decimals and correct the pools that hold it. Discovery
   * has to write a pool row before every currency has answered, so a pool can
   * carry a placeholder until the token itself is read.
   */
  const learn = (address: Address, decimals: number): void => {
    const key = address.toLowerCase();
    if (decimalsCache.get(key) === decimals) return;
    decimalsCache.set(key, decimals);
    try {
      const corrected = deps.store.applyTokenDecimals(key, decimals);
      if (corrected > 0) logger.info('pool decimals corrected', { token: key, pools: corrected, decimals });
    } catch (error) {
      logger.debug('pool decimals could not be corrected', { token: key, reason: messageOf(error) });
    }
  };

  const readDecimals = async (addresses: readonly Address[]): Promise<Map<string, number>> => {
    const wanted: Address[] = [];
    for (const address of addresses) {
      const key = address.toLowerCase();
      if (decimalsCache.has(key)) continue;
      const known = deps.registry.token(key) ?? deps.store.getToken(key);
      if (known) {
        decimalsCache.set(key, known.decimals);
        continue;
      }
      if (!wanted.some((entry) => entry.toLowerCase() === key)) wanted.push(address);
    }

    if (wanted.length > 0) {
      const calls: ContractCall[] = wanted.map((address) => ({
        address,
        abi: erc20Abi,
        functionName: 'decimals',
      }));
      const results = await deps.client.multicallAllowFailure<number>(calls);
      wanted.forEach((address, index) => {
        const outcome = results[index];
        if (outcome?.status === 'success') {
          learn(address, Number(outcome.result));
          return;
        }
        // A token that did not answer is left unresolved. Caching eighteen
        // here would price every pool holding a six-decimal token a trillion
        // out, permanently, and nothing would ever ask again.
        logger.debug('token did not answer decimals', { token: address });
      });
    }

    const out = new Map<string, number>();
    for (const address of addresses) {
      const key = address.toLowerCase();
      const known = decimalsCache.get(key);
      if (known !== undefined) out.set(key, known);
    }
    return out;
  };

  return {
    decimalsOf: readDecimals,

    async read(addresses) {
      if (addresses.length === 0) return [];
      const calls: ContractCall[] = [];
      for (const address of addresses) {
        for (const functionName of FIELDS)
          calls.push({ address, abi: stockTokenAbi, functionName });
      }
      const results = await deps.client.multicallAllowFailure<unknown>(calls);

      const tokens: Token[] = [];
      addresses.forEach((address, index) => {
        const slice = results.slice(index * FIELDS.length, (index + 1) * FIELDS.length);
        const known = deps.registry.token(address.toLowerCase());
        const token = assembleToken(address, slice, known);
        // A token that did not answer decimals is not recorded. Storing the
        // ERC-20 default would leave every later reader believing a number
        // nothing on chain confirmed, which is how a six-decimal token gets
        // priced a trillion out.
        if (slice[0]?.status !== 'success' && known?.decimals === undefined) {
          logger.debug('token did not answer decimals, so it was not recorded', { token: address });
          return;
        }
        learn(address, token.decimals);
        tokens.push(token);
        try {
          deps.store.upsertToken(token);
        } catch (error) {
          logger.warn('token could not be stored', { token: address, reason: messageOf(error) });
        }
      });
      return tokens;
    },

    async balanceOf(token, holder) {
      const [decimals, balance] = await Promise.all([
        readDecimals([token]).then((map) => map.get(token.toLowerCase()) ?? FALLBACK_DECIMALS),
        deps.client.publicClient.readContract({
          address: token,
          abi: erc20Abi,
          functionName: 'balanceOf',
          args: [holder],
        }) as Promise<bigint>,
      ]);
      return rawQuantity(balance, decimals, 'pool');
    },

    async balanceOfUi(token, holder) {
      const known = deps.registry.token(token.toLowerCase());
      if (known && !known.isStockToken) {
        throw new NotFoundError(`balanceOfUI on ${known.symbol}`, {
          token,
          reason: 'not a stock token',
        });
      }
      const balance = (await deps.client.publicClient.readContract({
        address: token,
        abi: stockTokenAbi,
        functionName: 'balanceOfUI',
        args: [holder],
      })) as bigint;
      const decimals = (await readDecimals([token])).get(token.toLowerCase()) ?? FALLBACK_DECIMALS;
      return { ...rawQuantity(balance, decimals, 'pool'), unit: 'share' };
    },
  };
}

/** Merge one token's multicall results with what the registry already knows. */
function assembleToken(
  address: Address,
  results: readonly CallOutcome<unknown>[],
  known: Token | undefined,
): Token {
  const value = <T>(index: number): T | undefined => {
    const outcome = results[index];
    return outcome?.status === 'success' ? (outcome.result as T) : undefined;
  };

  const decimals = Number(value<number | bigint>(0) ?? known?.decimals ?? 18);
  const multiplier = value<bigint>(3);
  const pending = value<bigint>(4);

  return {
    address: address.toLowerCase() as Address,
    symbol: value<string>(1) ?? known?.symbol ?? shortAddress(address),
    name: value<string>(2) ?? known?.name ?? '',
    decimals,
    isStockToken: multiplier !== undefined,
    uiMultiplier: multiplier === undefined ? known?.uiMultiplier : scaledMultiplier(multiplier),
    pendingMultiplier:
      pending === undefined || pending === multiplier
        ? known?.pendingMultiplier
        : scaledMultiplier(pending),
    tokenPaused: value<boolean>(5),
    oraclePaused: value<boolean>(6),
    feed: known?.feed,
    lighterMarketId: known?.lighterMarketId,
    tradingCapabilities: known?.tradingCapabilities,
    isin: known?.isin,
    logoUrl: known?.logoUrl,
  };
}

/** The multiplier is fixed point with 18 decimals: 1000566080061092436 is 1.000566. */
export function scaledMultiplier(raw: bigint): number {
  return Number(raw) / 10 ** MULTIPLIER_DECIMALS;
}

function shortAddress(address: Address): string {
  return `${address.slice(0, 6)}...${address.slice(-4)}`;
}

function messageOf(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}
