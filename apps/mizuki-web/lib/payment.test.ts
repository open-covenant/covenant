import { describe, expect, it, vi } from 'vitest';
import {
  checkQuotePaymentStatus,
  clearWorkbenchPaymentRecovery,
  loadWorkbenchPaymentRecovery,
  PaymentStatusError,
  quoteMatchesIssue,
  saveWorkbenchPaymentRecovery,
} from './payment';
import type { Quote } from './types';

describe('quoteMatchesIssue', () => {
  const quote = { owner: 'open-covenant', repo: 'covenant', issueNumber: 42 };

  it('accepts the canonical issue and harmless URL casing', () => {
    expect(quoteMatchesIssue(quote, 'https://github.com/open-covenant/covenant/issues/42')).toBe(
      true,
    );
    expect(quoteMatchesIssue(quote, 'https://github.com/Open-Covenant/Covenant/issues/42/')).toBe(
      true,
    );
  });

  it('rejects a different issue or non-issue URL', () => {
    expect(quoteMatchesIssue(quote, 'https://github.com/open-covenant/covenant/issues/43')).toBe(
      false,
    );
    expect(quoteMatchesIssue(quote, 'https://example.com/open-covenant/covenant/issues/42')).toBe(
      false,
    );
  });
});

describe('payment status recovery', () => {
  it('checks the existing record without a payment header, body, or wallet request', async () => {
    const request = vi.fn(async (_input: RequestInfo | URL, _init?: RequestInit) =>
      Response.json({
        paymentStatus: 'job_reserved',
        quoteId: quote.id,
        job: { id: 'job-1', state: 'settlement_pending' },
      }),
    );

    await expect(
      checkQuotePaymentStatus(quote.id, 'stable-payment-key', request as typeof fetch),
    ).resolves.toMatchObject({ status: 'job_reserved', job: { id: 'job-1' } });

    const [path, init] = request.mock.calls[0]!;
    const headers = new Headers(init?.headers);
    expect(path).toBe(`/api/mizuki/v1/account/quotes/${quote.id}/payment-status`);
    expect(init?.method).toBe('GET');
    expect(init?.body).toBeUndefined();
    expect(headers.get('idempotency-key')).toBe('stable-payment-key');
    expect(headers.get('payment-signature')).toBeNull();
  });

  it('distinguishes a still-payable quote from a reserved job', async () => {
    const request = vi.fn(async () =>
      Response.json({
        paymentStatus: 'unpaid',
        quoteId: quote.id,
        expiresAt: quote.expiresAt,
      }),
    );

    await expect(
      checkQuotePaymentStatus(quote.id, 'stable-payment-key', request as typeof fetch),
    ).resolves.toEqual({ status: 'unpaid', expiresAt: quote.expiresAt });
  });

  it('rejects account and quote-binding failures', async () => {
    const denied = vi.fn(async () => Response.json({ error: 'quote not found' }, { status: 404 }));
    await expect(
      checkQuotePaymentStatus(quote.id, 'stable-payment-key', denied as typeof fetch),
    ).rejects.toEqual(new PaymentStatusError('quote not found', 404));

    const mismatch = vi.fn(async () =>
      Response.json({
        paymentStatus: 'unpaid',
        quoteId: 'different-quote',
        expiresAt: quote.expiresAt,
      }),
    );
    await expect(
      checkQuotePaymentStatus(quote.id, 'stable-payment-key', mismatch as typeof fetch),
    ).rejects.toThrow('did not match the accepted quote');
  });
});

describe('Workbench payment recovery storage', () => {
  it('restores the exact accepted quote and removes it after resolution', () => {
    const storage = memoryStorage();
    const recovery = {
      phase: 'uncertain' as const,
      repository: 'open-covenant/covenant',
      issueUrl: quote.issueUrl,
      quote,
    };

    saveWorkbenchPaymentRecovery(recovery, storage);
    expect(loadWorkbenchPaymentRecovery(storage)).toEqual(recovery);
    clearWorkbenchPaymentRecovery(quote.id, storage);
    expect(loadWorkbenchPaymentRecovery(storage)).toBeNull();
  });

  it('discards recovery data that does not match the quote repository and issue', () => {
    const storage = memoryStorage();
    storage.setItem(
      'mizuki:workbench:payment-recovery',
      JSON.stringify({
        phase: 'uncertain',
        repository: 'attacker/other',
        issueUrl: quote.issueUrl,
        quote,
      }),
    );

    expect(loadWorkbenchPaymentRecovery(storage)).toBeNull();
    expect(storage.getItem('mizuki:workbench:payment-recovery')).toBeNull();
  });
});

const quote: Quote = {
  id: '11111111-1111-4111-8111-111111111111',
  issueUrl: 'https://github.com/open-covenant/covenant/issues/42',
  owner: 'open-covenant',
  repo: 'covenant',
  issueNumber: 42,
  issueTitle: 'Fix documentation',
  class: 'micro',
  priceAtomic: '2000000',
  maxFiles: 3,
  maxCostUsd: 0.8,
  expiresAt: '2099-01-01T00:00:00.000Z',
  payment: { x402Version: 2 },
};

function memoryStorage() {
  const values = new Map<string, string>();
  return {
    getItem: (key: string) => values.get(key) ?? null,
    setItem: (key: string, value: string) => values.set(key, value),
    removeItem: (key: string) => values.delete(key),
  };
}
