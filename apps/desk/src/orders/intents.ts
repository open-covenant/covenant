/**
 * Desk intents.
 *
 * An intent is the same order, written so that somebody else could fill it
 * inside the same limits: a token pair, a ceiling on what leaves the wallet, a
 * floor on what comes back, a deadline, and a hash of the conditions that
 * armed it. The desk signs intents. It does not run a filler, and nothing in
 * v1 redeems one. The format is documented in `INTENTS.md`.
 */

import { hashTypedData, keccak256, recoverTypedDataAddress, stringToHex } from 'viem';
import { privateKeyToAccount } from 'viem/accounts';
import { KeystoreError } from '../core/errors.js';
import type { Address, Order } from '../core/types.js';
import type { DeskIntent, DeskIntentTypedData, Intents } from './types.js';

/** EIP-712 domain name. Stable across versions of the desk. */
export const INTENT_DOMAIN_NAME = 'Covenant Desk';
/** EIP-712 domain version. Bump only when the field list changes. */
export const INTENT_DOMAIN_VERSION = '1';

export const INTENT_TYPES = {
  DeskIntent: [
    { name: 'tokenIn', type: 'address' },
    { name: 'tokenOut', type: 'address' },
    { name: 'maxAmountIn', type: 'uint256' },
    { name: 'minAmountOut', type: 'uint256' },
    { name: 'deadline', type: 'uint256' },
    { name: 'conditionsHash', type: 'bytes32' },
  ],
} as const;

/** Default life of a signed intent, seconds. */
export const DEFAULT_INTENT_TTL_SEC = 300;

export interface IntentsDeps {
  readonly chainId: number;
  /** Returns the desk signing key. Called only when a signature is needed. */
  readonly privateKey: () => string;
  readonly now?: () => number;
}

export function createIntents(deps: IntentsDeps): Intents {
  const clock = deps.now ?? Date.now;
  const domain = {
    name: INTENT_DOMAIN_NAME,
    version: INTENT_DOMAIN_VERSION,
    chainId: deps.chainId,
  } as const;

  const account = () => privateKeyToAccount(normaliseKey(deps.privateKey()));

  return {
    typedData(intent: DeskIntent): DeskIntentTypedData {
      return { domain, types: INTENT_TYPES, primaryType: 'DeskIntent', message: intent };
    },

    conditionsHash(order: Order): `0x${string}` {
      return keccak256(stringToHex(canonicalConditions(order)));
    },

    hash(intent: DeskIntent): `0x${string}` {
      return hashTypedData({ domain, types: INTENT_TYPES, primaryType: 'DeskIntent', message: intent });
    },

    async sign(intent: DeskIntent): Promise<`0x${string}`> {
      return account().signTypedData({ domain, types: INTENT_TYPES, primaryType: 'DeskIntent', message: intent });
    },

    async verify(intent: DeskIntent, signature: `0x${string}`, expected: Address): Promise<boolean> {
      try {
        const signer = await recoverTypedDataAddress({
          domain,
          types: INTENT_TYPES,
          primaryType: 'DeskIntent',
          message: intent,
          signature,
        });
        return signer.toLowerCase() === expected.toLowerCase();
      } catch {
        return false;
      }
    },

    signerAddress(): Address {
      return account().address;
    },

    fromOrder(order, options): DeskIntent {
      const now = options.now ?? clock();
      const ttl = options.deadlineSec ?? DEFAULT_INTENT_TTL_SEC;
      return {
        tokenIn: order.tokenIn,
        tokenOut: order.tokenOut,
        maxAmountIn: order.amountIn,
        minAmountOut: options.minAmountOut,
        deadline: BigInt(Math.floor(now / 1000) + ttl),
        conditionsHash: keccak256(stringToHex(canonicalConditions(order))),
      };
    },
  };
}

/**
 * The conditions a filler must respect, in a fixed field order so the hash is
 * reproducible. Status and timestamps that change after creation are left out.
 */
export function canonicalConditions(order: Order): string {
  const trigger = order.trigger;
  return JSON.stringify([
    order.id,
    order.kind,
    order.side,
    order.tokenIn.toLowerCase(),
    order.tokenOut.toLowerCase(),
    order.amountIn.toString(),
    [
      trigger.priceLte ?? null,
      trigger.priceGte ?? null,
      trigger.priceBasis ?? 'usdOnchain',
      trigger.premiumLteBps ?? null,
      trigger.premiumGteBps ?? null,
      trigger.atNextOpenOffsetSec ?? null,
      trigger.at ?? null,
    ],
    [order.bounds.maxSlippageBps, order.bounds.maxOrderNotionalUsd, order.bounds.maxBuyPremiumBps],
    order.live,
    order.expiresAt,
    order.parentId,
  ]);
}

/** Accept a key with or without the `0x` prefix. The value is never logged or returned. */
function normaliseKey(value: string): `0x${string}` {
  const trimmed = value.trim();
  const prefixed = trimmed.startsWith('0x') ? trimmed : `0x${trimmed}`;
  if (!/^0x[0-9a-fA-F]{64}$/.test(prefixed)) {
    throw new KeystoreError(
      'DESK_EVM_PRIVATE_KEY is not a 32-byte hex key. Check the value in keys.env and start again.',
      { name: 'DESK_EVM_PRIVATE_KEY' },
    );
  }
  return prefixed as `0x${string}`;
}
