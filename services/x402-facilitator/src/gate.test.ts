import { describe, expect, it } from 'vitest';
import { declareDiscoveryExtension } from '@x402/extensions/bazaar';
import type { PaymentPayload, PaymentRequirements, SettleResponse, VerifyResponse } from '@x402/core/types';
import { BASE_MAINNET, SOLANA_MAINNET, USDC } from './config.js';
import { Gate, type FacilitatorCore } from './gate.js';
import { Store } from './store.js';

const SELLER = '0x5fA1d0C0bfFE257a20027C523093F941834f5D66';
const BUYER = '0xe52e0C71783A2d5c7fB2e695f0faAEe839d8fe45';

function requirements(overrides: Partial<PaymentRequirements> = {}): PaymentRequirements {
  return {
    scheme: 'exact',
    network: BASE_MAINNET,
    asset: USDC[BASE_MAINNET]!,
    amount: '14304',
    payTo: SELLER,
    maxTimeoutSeconds: 300,
    extra: { name: 'USDC', version: '2' },
    ...overrides,
  } as PaymentRequirements;
}

function payload(accepted: PaymentRequirements, extensions?: Record<string, unknown>, nonce = '0x01'): PaymentPayload {
  return {
    x402Version: 2,
    resource: {
      url: 'https://compute-inference.opencovenant.org/v1/chat/completions',
      description: 'Text inference',
      mimeType: 'application/json',
      serviceName: 'Covenant Compute',
    },
    accepted,
    payload: { authorization: { from: BUYER, nonce } },
    ...(extensions ? { extensions } : {}),
  } as unknown as PaymentPayload;
}

class FakeCore implements FacilitatorCore {
  verifies = 0;
  settles = 0;
  payer = BUYER;
  async verify(): Promise<VerifyResponse> {
    this.verifies += 1;
    return { isValid: true, payer: this.payer };
  }
  async settle(p: PaymentPayload, r: PaymentRequirements): Promise<SettleResponse> {
    this.settles += 1;
    const nonce = (p.payload as { authorization: { nonce: string } }).authorization.nonce;
    return { success: true, transaction: `0xtx${nonce}`, network: r.network, payer: this.payer };
  }
}

function gate(overrides: Partial<ConstructorParameters<typeof Gate>[2]> = {}, now = () => Date.UTC(2026, 9, 22)) {
  const core = new FakeCore();
  const store = new Store(':memory:');
  const instance = new Gate(
    core,
    store,
    { minAmount: 1_000n, dailySettlements: 3, dailySettlementsPerPayTo: 2, unmeteredPayTo: [], ...overrides },
    new Set([BASE_MAINNET, SOLANA_MAINNET]),
    () => {},
    now,
  );
  return { core, store, gate: instance };
}

const { bazaar } = declareDiscoveryExtension({
  method: 'POST',
  bodyType: 'json',
  input: { model: 'claude-haiku-4.5', messages: [{ role: 'user', content: 'Hi' }], max_tokens: 64 },
  inputSchema: { properties: { model: { type: 'string' } }, required: ['model'] },
  output: { example: { choices: [] } },
});

describe('payment checks', () => {
  it.each([
    ['an unsupported network', requirements({ network: 'eip155:1' }), 'invalid_network'],
    ['an asset other than USDC', requirements({ asset: '0x4200000000000000000000000000000000000006' }), 'unsupported_asset'],
    ['an amount below the floor', requirements({ amount: '999' }), 'amount_below_minimum'],
    ['a malformed amount', requirements({ amount: '1e9' }), 'invalid_amount'],
    ['another scheme', requirements({ scheme: 'upto' }), 'unsupported_scheme'],
  ])('refuses %s before spending anything', async (_name, terms, reason) => {
    const { core, gate: instance } = gate();
    await expect(instance.verify(payload(terms), terms)).resolves.toEqual({ isValid: false, invalidReason: reason });
    await expect(instance.settle(payload(terms), terms)).resolves.toMatchObject({
      response: { success: false, errorReason: reason },
    });
    expect(core.verifies + core.settles).toBe(0);
  });

  it('accepts USDC on Solana by its mint', async () => {
    const terms = requirements({ network: SOLANA_MAINNET, asset: USDC[SOLANA_MAINNET]!, payTo: 'Seller1111' });
    const { gate: instance } = gate();
    await expect(instance.verify(payload(terms), terms)).resolves.toMatchObject({ isValid: true });
  });

  it('refuses a payment to oneself, with addresses compared case-insensitively on EVM', async () => {
    const { core, gate: instance } = gate();
    core.payer = SELLER.toLowerCase();
    const terms = requirements();
    await expect(instance.verify(payload(terms), terms)).resolves.toMatchObject({
      isValid: false,
      invalidReason: 'self_send_not_allowed',
    });
    await expect(instance.settle(payload(terms), terms)).resolves.toMatchObject({
      response: { success: false, errorReason: 'self_send_not_allowed' },
    });
    expect(core.settles).toBe(0);
  });

  it('settles a payment verified moments ago without verifying it again', async () => {
    const { core, gate: instance } = gate();
    const terms = requirements();
    await instance.verify(payload(terms), terms);
    await instance.settle(payload(terms), terms);
    expect(core.verifies).toBe(1);
    // The remembered payer is used once; a second settle verifies again.
    await instance.settle(payload(terms), terms);
    expect(core.verifies).toBe(2);
  });
});

describe('daily caps', () => {
  it('stops one receiver at its cap and the network at the overall cap', async () => {
    const { core, store, gate: instance } = gate();
    const settle = (payTo: string, nonce: string) => {
      const terms = requirements({ payTo });
      return instance.settle(payload(terms, undefined, nonce), terms);
    };
    await expect(settle(SELLER, '0x01')).resolves.toMatchObject({ response: { success: true } });
    await expect(settle(SELLER, '0x02')).resolves.toMatchObject({ response: { success: true } });
    await expect(settle(SELLER, '0x03')).resolves.toMatchObject({
      response: { success: false, errorReason: 'pay_to_daily_limit_reached' },
    });
    const other = '0x1111111111111111111111111111111111111111';
    await expect(settle(other, '0x04')).resolves.toMatchObject({ response: { success: true } });
    await expect(settle(other, '0x05')).resolves.toMatchObject({
      response: { success: false, errorReason: 'facilitator_daily_limit_reached' },
    });
    expect(core.settles).toBe(3);
    expect(store.settlementTotals()).toEqual([{ network: BASE_MAINNET, settlements: 3 }]);
  });

  it('lets unmetered receivers through and does not count them', async () => {
    const { store, gate: instance } = gate({ dailySettlements: 0, unmeteredPayTo: [SELLER.toLowerCase()] });
    const terms = requirements();
    await expect(instance.settle(payload(terms), terms)).resolves.toMatchObject({ response: { success: true } });
    expect(store.meteredSettlements(BASE_MAINNET, '2026-10-22')).toBe(0);
  });

  it('starts a new count each UTC day', async () => {
    let now = Date.UTC(2026, 9, 22, 23, 59);
    const { gate: instance } = gate({ dailySettlementsPerPayTo: 1 }, () => now);
    const terms = requirements();
    await instance.settle(payload(terms, undefined, '0x01'), terms);
    now = Date.UTC(2026, 9, 23, 0, 1);
    await expect(instance.settle(payload(terms, undefined, '0x02'), terms)).resolves.toMatchObject({
      response: { success: true },
    });
  });
});

describe('Bazaar catalog', () => {
  it('catalogs a settled payment that declares a valid listing', async () => {
    const { store, gate: instance } = gate();
    const terms = requirements();
    const outcome = await instance.settle(payload(terms, { bazaar }), terms);
    expect(outcome.bazaar).toEqual({ status: 'success' });
    const [item] = store.catalog({});
    expect(item).toMatchObject({
      resource: 'https://compute-inference.opencovenant.org/v1/chat/completions',
      type: 'http',
      serviceName: 'Covenant Compute',
      accepts: [terms],
      extensions: { bazaar },
    });
  });

  it('rejects a listing whose info fails its own schema, and still settles', async () => {
    const { store, gate: instance } = gate();
    const terms = requirements();
    const broken = { ...bazaar, info: { input: { type: 'http', method: 'POST' } } };
    const outcome = await instance.settle(payload(terms, { bazaar: broken }), terms);
    expect(outcome.response.success).toBe(true);
    expect(outcome.bazaar).toMatchObject({ status: 'rejected' });
    expect(store.catalog({})).toEqual([]);
  });

  it('keeps one entry per resource and merges the rails it was paid on', async () => {
    const { store, gate: instance } = gate();
    const base = requirements();
    const solana = requirements({ network: SOLANA_MAINNET, asset: USDC[SOLANA_MAINNET]!, payTo: 'Seller1111' });
    await instance.settle(payload(base, { bazaar }, '0x01'), base);
    await instance.settle(payload(solana, { bazaar }, '0x02'), solana);
    const items = store.catalog({});
    expect(items).toHaveLength(1);
    expect(items[0]!.accepts.map((a) => a.network)).toEqual([SOLANA_MAINNET, BASE_MAINNET]);
    expect(store.catalog({ network: BASE_MAINNET })).toHaveLength(1);
    expect(store.catalog({ payTo: SELLER.toLowerCase() })).toHaveLength(1);
    expect(store.catalog({ network: 'eip155:1' })).toHaveLength(0);
    expect(store.catalog({}, ['covenant', 'inference'])).toHaveLength(1);
    expect(store.catalog({}, ['weather'])).toHaveLength(0);
  });
});
