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
  settleSucceeds = true;
  /** Resolves pending settles when called; settles run immediately while it is unset. */
  hold?: Promise<void>;
  async verify(): Promise<VerifyResponse> {
    this.verifies += 1;
    return { isValid: true, payer: this.payer, invalidMessage: 'rpc https://rpc.example/?api-key=secret' } as VerifyResponse;
  }
  async settle(p: PaymentPayload, r: PaymentRequirements): Promise<SettleResponse> {
    this.settles += 1;
    await this.hold;
    const nonce = (p.payload as { authorization: { nonce: string } }).authorization.nonce;
    return this.settleSucceeds
      ? { success: true, transaction: `0xtx${nonce}`, network: r.network, payer: this.payer }
      : ({
          success: false,
          errorReason: 'transaction_failed',
          errorMessage: 'rpc https://rpc.example/?api-key=secret',
          transaction: `0xtx${nonce}`,
          network: r.network,
          payer: this.payer,
        } as SettleResponse);
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

const page = { limit: 20, offset: 0 };

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
    const terms = requirements({ extra: { name: 'USDC', version: '2', quoteId: 'q-1', requestSha256: 'ab'.repeat(32) } });
    const outcome = await instance.settle(payload(terms, { bazaar }), terms);
    expect(outcome.bazaar).toEqual({ status: 'success' });
    const [item] = store.catalog({}, page).items;
    expect(item).toMatchObject({
      resource: 'https://compute-inference.opencovenant.org/v1/chat/completions',
      type: 'http',
      serviceName: 'Covenant Compute',
      accepts: [{ ...terms, extra: { name: 'USDC', version: '2' } }],
      extensions: { bazaar },
    });
  });

  it('rejects a listing that does not follow the bazaar spec, and still settles', async () => {
    const { store, gate: instance } = gate();
    const terms = requirements();
    const broken = { ...bazaar, info: { input: { type: 'ftp', method: 'POST' } } };
    const outcome = await instance.settle(payload(terms, { bazaar: broken }), terms);
    expect(outcome.response.success).toBe(true);
    expect(outcome.bazaar).toMatchObject({ status: 'rejected' });
    expect(store.catalog({}, page).items).toEqual([]);
  });

  it('keeps a separate entry per receiving rail, so one seller cannot rewrite another', async () => {
    const { store, gate: instance } = gate();
    const base = requirements();
    const other = requirements({ payTo: '0x1111111111111111111111111111111111111111' });
    const solana = requirements({ network: SOLANA_MAINNET, asset: USDC[SOLANA_MAINNET]!, payTo: 'Seller1111' });
    await instance.settle(payload(base, { bazaar }, '0x01'), base);
    await instance.settle(payload(other, { bazaar }, '0x02'), other);
    await instance.settle(payload(solana, { bazaar }, '0x03'), solana);
    await instance.settle(payload(base, { bazaar }, '0x04'), base);
    expect(store.catalog({}, page).total).toBe(3);
    expect(store.catalog({ network: BASE_MAINNET }, page).total).toBe(2);
    expect(store.catalog({ payTo: SELLER.toLowerCase() }, page).items[0]!.accepts[0]!.payTo).toBe(SELLER);
    expect(store.catalog({ network: 'eip155:1' }, page).total).toBe(0);
    expect(store.catalog({ extensions: 'bazaar' }, page).total).toBe(3);
    expect(store.catalog({}, page, ['covenant', 'inference']).total).toBe(3);
    expect(store.catalog({}, page, ['weather']).total).toBe(0);
    expect(store.catalog({}, { limit: 1, offset: 1 }).items).toHaveLength(1);
  });

  it('never compiles the schema a payment carries', async () => {
    const { store, gate: instance } = gate();
    const terms = requirements();
    // Ajv would run this pattern on the request thread; the gate only checks the spec's own rules.
    const hostile = {
      ...bazaar,
      schema: { type: 'object', properties: { input: { type: 'object', properties: { method: { type: 'string', pattern: '^(a+)+$' } } } } },
    };
    const started = Date.now();
    const outcome = await instance.settle(payload(terms, { bazaar: hostile }), terms);
    expect(Date.now() - started).toBeLessThan(1_000);
    expect(outcome.bazaar).toEqual({ status: 'success' });
    expect(store.catalog({}, page).total).toBe(1);
  });

  it('reports a listing problem without failing a settled payment', async () => {
    const { gate: instance } = gate();
    const terms = requirements();
    const mcp = payload(terms, { bazaar });
    (mcp.resource as { url: string }).url = 'mcp://tools.example/run';
    const outcome = await instance.settle(mcp, terms);
    expect(outcome.response.success).toBe(true);
    expect(outcome.bazaar).toMatchObject({ status: 'rejected' });
  });
});

describe('what a settlement costs the signer', () => {
  it('counts a broadcast that failed on chain against the caps, but not in the totals', async () => {
    const { core, store, gate: instance } = gate({ dailySettlementsPerPayTo: 1 });
    core.settleSucceeds = false;
    const terms = requirements();
    const first = await instance.settle(payload(terms, undefined, '0x01'), terms);
    expect(first.response).toEqual({
      success: false,
      errorReason: 'transaction_failed',
      transaction: '0xtx0x01',
      network: BASE_MAINNET,
      payer: BUYER,
    });
    await expect(instance.settle(payload(terms, undefined, '0x02'), terms)).resolves.toMatchObject({
      response: { errorReason: 'pay_to_daily_limit_reached' },
    });
    expect(store.settlementTotals()).toEqual([]);
  });

  it('settles one payment per payer at a time', async () => {
    const { core, gate: instance } = gate();
    let release!: () => void;
    core.hold = new Promise((resolve) => (release = resolve));
    const terms = requirements();
    const first = instance.settle(payload(terms, undefined, '0x01'), terms);
    await new Promise((resolve) => setImmediate(resolve));
    await expect(instance.settle(payload(terms, undefined, '0x02'), terms)).resolves.toMatchObject({
      response: { success: false, errorReason: 'payer_settlement_in_progress' },
    });
    release();
    await expect(first).resolves.toMatchObject({ response: { success: true } });
    core.hold = undefined;
    await expect(instance.settle(payload(terms, undefined, '0x03'), terms)).resolves.toMatchObject({
      response: { success: true },
    });
  });

  it('cannot be pushed past a cap by concurrent settlements from different payers', async () => {
    const { core, gate: instance } = gate({ dailySettlementsPerPayTo: 2 });
    let release!: () => void;
    core.hold = new Promise((resolve) => (release = resolve));
    const settle = (n: number) => {
      const terms = requirements();
      const p = payload(terms, undefined, `0x0${n}`);
      (p.payload as { authorization: { from: string } }).authorization.from = `0x${String(n).repeat(40)}`;
      core.payer = `0x${String(n).repeat(40)}`;
      return instance.settle(p, terms);
    };
    const results = [settle(1), settle(2), settle(3), settle(4)];
    await new Promise((resolve) => setImmediate(resolve));
    release();
    const outcomes = await Promise.all(results);
    expect(outcomes.filter((o) => o.response.success)).toHaveLength(2);
    expect(outcomes.filter((o) => o.response.errorReason === 'pay_to_daily_limit_reached')).toHaveLength(2);
  });

  it('gives a receiver one quota whatever the case of its address', async () => {
    const { gate: instance } = gate({ dailySettlementsPerPayTo: 1 });
    const lower = requirements({ payTo: SELLER.toLowerCase() });
    const upper = requirements({ payTo: SELLER.toUpperCase().replace('0X', '0x') });
    await instance.settle(payload(lower, undefined, '0x01'), lower);
    await expect(instance.settle(payload(upper, undefined, '0x02'), upper)).resolves.toMatchObject({
      response: { errorReason: 'pay_to_daily_limit_reached' },
    });
  });

  it('never passes RPC error text back to the caller', async () => {
    const { gate: instance } = gate();
    const terms = requirements();
    expect(JSON.stringify(await instance.verify(payload(terms), terms))).not.toContain('api-key');
  });
});
