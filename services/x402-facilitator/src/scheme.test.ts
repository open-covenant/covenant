import { describe, expect, it, vi } from 'vitest';
import { createLighthouseTolerantScheme } from './scheme.js';
import {
  AMOUNT,
  buildPayment,
  computeLimit,
  computePrice,
  guard,
  memo,
  transferChecked,
  USDC,
} from './test-fixtures.js';

/**
 * These drive the real ExactSvmScheme with a stub signer and assert on the
 * call log, because the defect they cover was invisible from the response
 * alone: verification returned valid while never simulating, and settlement
 * broadcasts with skipPreflight, so the fee payer pays for a transaction that
 * cannot succeed.
 */
const NETWORK = 'solana:5eykt4UsFv8P8NJdTREpY1vzqKqZKvdp';

function stubSigner(calls: string[], feePayer: string, simulate: () => void = () => {}) {
  return {
    getAddresses: () => [feePayer],
    getSigner: () => ({}),
    signTransaction: vi.fn(async () => {
      calls.push('sign');
      return 'signed';
    }),
    sendTransaction: vi.fn(async () => {
      calls.push('send');
      return 'signature';
    }),
    confirmTransaction: vi.fn(async () => {
      calls.push('confirm');
    }),
    simulateTransaction: vi.fn(async () => {
      calls.push('simulate');
      simulate();
    }),
    getTokenAccountBalance: vi.fn(async () => null),
    fetchAddressLookupTables: vi.fn(async () => ({})),
    getConfirmedTransactionInnerInstructions: vi.fn(async () => null),
    simulateTransactionWithInnerInstructions: vi.fn(async () => ({ innerInstructions: [] })),
  };
}

function requirementsFor(terms: { payTo: string; feePayer: string; memo?: string }) {
  return {
    scheme: 'exact',
    network: NETWORK,
    amount: String(AMOUNT),
    asset: USDC,
    payTo: terms.payTo,
    maxTimeoutSeconds: 60,
    extra: { feePayer: terms.feePayer, ...(terms.memo ? { memo: terms.memo } : {}) },
  };
}

function payloadFor(transaction: string, requirements: unknown) {
  return {
    x402Version: 2,
    resource: { url: 'https://mizuki.test/job', description: 'job', mimeType: 'application/json' },
    accepted: requirements,
    payload: { transaction },
  };
}

async function guardedPayment() {
  return buildPayment(({ source, destination, payer }) => [
    computeLimit(),
    computePrice(),
    guard(17),
    guard(52),
    transferChecked(source, destination, payer),
    memo(),
  ]);
}

async function plainPayment() {
  return buildPayment(({ source, destination, payer }) => [
    computeLimit(),
    computePrice(),
    transferChecked(source, destination, payer),
    memo(),
  ]);
}

describe('wallet-guard fallback safety', () => {
  it('simulates a guarded payment before reporting it valid', async () => {
    const calls: string[] = [];
    const { transaction, terms } = await guardedPayment();
    const signer = stubSigner(calls, terms.feePayer);
    const scheme = createLighthouseTolerantScheme(signer as never, undefined as never);
    const requirements = requirementsFor(terms);

    const result = await scheme.verify(
      payloadFor(transaction, requirements) as never,
      requirements as never,
    );

    expect(result.isValid).toBe(true);
    expect(calls).toContain('simulate');
  });

  it('refuses a guarded payment whose simulation fails', async () => {
    const calls: string[] = [];
    const { transaction, terms } = await guardedPayment();
    const signer = stubSigner(calls, terms.feePayer, () => {
      throw new Error('Simulation failed: insufficient funds');
    });
    const scheme = createLighthouseTolerantScheme(signer as never, undefined as never);
    const requirements = requirementsFor(terms);

    const result = await scheme.verify(
      payloadFor(transaction, requirements) as never,
      requirements as never,
    );

    expect(result.isValid).toBe(false);
    expect(calls).not.toContain('sign');
    expect(calls).not.toContain('send');
  });

  it('does not override the library rejecting an unguarded payment on simulation', async () => {
    const calls: string[] = [];
    const { transaction, terms } = await plainPayment();
    const signer = stubSigner(calls, terms.feePayer, () => {
      throw new Error('Simulation failed: insufficient funds');
    });
    const scheme = createLighthouseTolerantScheme(signer as never, undefined as never);
    const requirements = requirementsFor(terms);

    const result = await scheme.verify(
      payloadFor(transaction, requirements) as never,
      requirements as never,
    );

    expect(result.isValid).toBe(false);
  });

  it('never signs or broadcasts a payment it could not verify', async () => {
    const calls: string[] = [];
    const { transaction, terms } = await guardedPayment();
    const signer = stubSigner(calls, terms.feePayer, () => {
      throw new Error('Simulation failed: insufficient funds');
    });
    const scheme = createLighthouseTolerantScheme(signer as never, undefined as never);
    const requirements = requirementsFor(terms);

    const settled = (await scheme.settle(
      payloadFor(transaction, requirements) as never,
      requirements as never,
    )) as { success: boolean };

    expect(settled.success).toBe(false);
    expect(calls).not.toContain('sign');
    expect(calls).not.toContain('send');
  });

  it('refuses a guarded payment that pays the wrong recipient', async () => {
    const calls: string[] = [];
    const { transaction, terms } = await guardedPayment();
    const signer = stubSigner(calls, terms.feePayer);
    const scheme = createLighthouseTolerantScheme(signer as never, undefined as never);
    const requirements = requirementsFor({
      ...terms,
      payTo: 'HN7cABqLq46Es1jh92dQQisAq662SmxELLLsHHe4YWrH',
    });

    const result = await scheme.verify(
      payloadFor(transaction, requirements) as never,
      requirements as never,
    );

    expect(result.isValid).toBe(false);
    expect(calls).not.toContain('sign');
  });
});
