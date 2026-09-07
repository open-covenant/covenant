import { describe, expect, it } from 'vitest';
import { generatePrivateKey, privateKeyToAccount } from 'viem/accounts';
import { isDeskError } from '../../src/core/errors.js';
import type { Order } from '../../src/core/types.js';
import { canonicalConditions, createIntents, INTENT_TYPES } from '../../src/orders/index.js';
import { AI, T0, USDG } from './harness.js';

const key = generatePrivateKey();
const account = privateKeyToAccount(key);
const intents = createIntents({ chainId: 4663, privateKey: () => key, now: () => T0 });

const order: Order = {
  id: 'ord_fixed',
  kind: 'limit',
  side: 'buy',
  tokenIn: USDG,
  tokenOut: AI,
  amountIn: 100n * 10n ** 6n,
  trigger: { priceLte: 2.5 },
  bounds: { maxSlippageBps: 100, maxOrderNotionalUsd: 250, maxBuyPremiumBps: 500 },
  live: false,
  status: 'open',
  createdAt: T0,
  expiresAt: null,
  parentId: null,
};

describe('intent typed data', () => {
  it('names the domain, the type, and the six fields a filler reads', () => {
    const intent = intents.fromOrder(order, { minAmountOut: 49n * 10n ** 18n });
    const typed = intents.typedData(intent);

    expect(typed.domain).toEqual({ name: 'Covenant Desk', version: '1', chainId: 4663 });
    expect(typed.primaryType).toBe('DeskIntent');
    expect(typed.types.DeskIntent?.map((field) => field.name)).toEqual([
      'tokenIn',
      'tokenOut',
      'maxAmountIn',
      'minAmountOut',
      'deadline',
      'conditionsHash',
    ]);
    expect(typed.message).toBe(intent);
    expect(INTENT_TYPES.DeskIntent).toHaveLength(6);
  });

  it('carries the order amount as the ceiling and the deadline five minutes out', () => {
    const intent = intents.fromOrder(order, { minAmountOut: 1n });
    expect(intent.maxAmountIn).toBe(order.amountIn);
    expect(intent.minAmountOut).toBe(1n);
    expect(intent.deadline).toBe(BigInt(Math.floor(T0 / 1000) + 300));
  });

  it('accepts a deadline the caller sets', () => {
    const intent = intents.fromOrder(order, { minAmountOut: 1n, deadlineSec: 30, now: T0 });
    expect(intent.deadline).toBe(BigInt(Math.floor(T0 / 1000) + 30));
  });
});

describe('the conditions hash', () => {
  it('is the same for the same order and changes when a bound moves', () => {
    const first = intents.conditionsHash(order);
    expect(first).toMatch(/^0x[0-9a-f]{64}$/);
    expect(intents.conditionsHash({ ...order })).toBe(first);

    const wider = intents.conditionsHash({
      ...order,
      bounds: { ...order.bounds, maxSlippageBps: 300 },
    });
    expect(wider).not.toBe(first);
  });

  it('changes when the trigger changes and ignores the status and the reason', () => {
    const base = intents.conditionsHash(order);
    expect(intents.conditionsHash({ ...order, trigger: { priceLte: 2.6 } })).not.toBe(base);
    expect(intents.conditionsHash({ ...order, status: 'triggered', reason: 'Anything.' })).toBe(base);
  });

  it('writes the default price basis into the canonical form, so it cannot drift', () => {
    expect(canonicalConditions(order)).toContain('usdOnchain');
    expect(canonicalConditions(order)).toBe(
      canonicalConditions({ ...order, trigger: { priceLte: 2.5, priceBasis: 'usdOnchain' } }),
    );
  });
});

describe('signing', () => {
  it('signs and verifies against the desk address', async () => {
    const intent = intents.fromOrder(order, { minAmountOut: 49n * 10n ** 18n });
    const signature = await intents.sign(intent);

    expect(signature).toMatch(/^0x[0-9a-f]+$/);
    expect(intents.signerAddress().toLowerCase()).toBe(account.address.toLowerCase());
    expect(await intents.verify(intent, signature, account.address)).toBe(true);
  });

  it('refuses a signature from another key and a payload that was edited after signing', async () => {
    const intent = intents.fromOrder(order, { minAmountOut: 49n * 10n ** 18n });
    const signature = await intents.sign(intent);
    const stranger = privateKeyToAccount(generatePrivateKey()).address;

    expect(await intents.verify(intent, signature, stranger)).toBe(false);
    expect(await intents.verify({ ...intent, minAmountOut: 1n }, signature, account.address)).toBe(false);
    expect(await intents.verify(intent, '0xdeadbeef', account.address)).toBe(false);
  });

  it('hashes the payload the same way twice and differently for a different payload', () => {
    const intent = intents.fromOrder(order, { minAmountOut: 2n });
    expect(intents.hash(intent)).toBe(intents.hash({ ...intent }));
    expect(intents.hash(intent)).not.toBe(intents.hash({ ...intent, deadline: intent.deadline + 1n }));
  });

  it('says what to do when the key is not a 32-byte hex value', async () => {
    const broken = createIntents({ chainId: 4663, privateKey: () => 'not-a-key' });
    const intent = intents.fromOrder(order, { minAmountOut: 1n });
    try {
      await broken.sign(intent);
      throw new Error('the intent was signed');
    } catch (error) {
      expect(isDeskError(error)).toBe(true);
      if (isDeskError(error)) {
        expect(error.code).toBe('keystore_missing');
        expect(error.reason).toContain('keys.env');
        expect(error.reason).not.toContain('not-a-key');
      }
    }
  });

  it('never puts the key in the signature path output', async () => {
    const intent = intents.fromOrder(order, { minAmountOut: 1n });
    const signature = await intents.sign(intent);
    expect(signature.includes(key.slice(2))).toBe(false);
  });
});
