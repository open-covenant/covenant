import { describe, expect, it, vi } from 'vitest';
import {
  checkQuotePaymentStatus,
  clearWorkbenchPaymentRecovery,
  loadWorkbenchPaymentRecovery,
  paymentAccountId,
  PaymentRecoveryStorageError,
  PaymentStatusError,
  prepareWorkbenchPaymentRecovery,
  issueMatchesRepository,
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

describe('issueMatchesRepository', () => {
  it('accepts only issues from the selected repository', () => {
    expect(
      issueMatchesRepository(
        'https://github.com/Open-Covenant/Covenant/issues/42/',
        'open-covenant/covenant',
      ),
    ).toBe(true);
    expect(
      issueMatchesRepository(
        'https://github.com/open-covenant/other/issues/42',
        'open-covenant/covenant',
      ),
    ).toBe(false);
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
      checkQuotePaymentStatus(quote.id, 'stable-payment-key', {
        request: request as typeof fetch,
      }),
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
      checkQuotePaymentStatus(quote.id, 'stable-payment-key', {
        request: request as typeof fetch,
      }),
    ).resolves.toEqual({ status: 'unpaid', expiresAt: quote.expiresAt });
  });

  it('rejects account and quote-binding failures', async () => {
    const denied = vi.fn(async () => Response.json({ error: 'quote not found' }, { status: 404 }));
    await expect(
      checkQuotePaymentStatus(quote.id, 'stable-payment-key', {
        request: denied as typeof fetch,
      }),
    ).rejects.toEqual(new PaymentStatusError('quote not found', 404));

    const mismatch = vi.fn(async () =>
      Response.json({
        paymentStatus: 'unpaid',
        quoteId: 'different-quote',
        expiresAt: quote.expiresAt,
      }),
    );
    await expect(
      checkQuotePaymentStatus(quote.id, 'stable-payment-key', {
        request: mismatch as typeof fetch,
      }),
    ).rejects.toThrow('did not match the accepted quote');
  });
});

describe('Workbench payment recovery storage', () => {
  it('stores the quote, account, phase, and idempotency key as one verified record', () => {
    const storage = memoryStorage();
    const recovery = {
      phase: 'uncertain' as const,
      accountId: '42',
      idempotencyKey: 'aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa',
      repository: 'open-covenant/covenant',
      issueUrl: quote.issueUrl,
      quote,
    };

    saveWorkbenchPaymentRecovery(recovery, storage);
    expect(loadWorkbenchPaymentRecovery('42', storage)).toEqual(recovery);
    clearWorkbenchPaymentRecovery('42', quote.id, storage);
    expect(loadWorkbenchPaymentRecovery('42', storage)).toBeNull();
  });

  it('discards recovery data that does not match the quote repository and issue', () => {
    const storage = memoryStorage();
    storage.setItem(
      'mizuki:workbench:payment-recovery',
      JSON.stringify({
        phase: 'uncertain',
        accountId: '42',
        idempotencyKey: 'aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa',
        repository: 'attacker/other',
        issueUrl: quote.issueUrl,
        quote,
      }),
    );

    expect(loadWorkbenchPaymentRecovery('42', storage)).toBeNull();
    expect(storage.getItem('mizuki:workbench:payment-recovery')).toBeNull();
  });

  it('does not expose or clear another signed-in account recovery record', () => {
    const storage = memoryStorage();
    const recovery = {
      phase: 'attempting' as const,
      accountId: '42',
      idempotencyKey: 'aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa',
      repository: 'open-covenant/covenant',
      issueUrl: quote.issueUrl,
      quote,
    };

    saveWorkbenchPaymentRecovery(recovery, storage);
    expect(loadWorkbenchPaymentRecovery('7', storage)).toBeNull();
    clearWorkbenchPaymentRecovery('7', quote.id, storage);
    expect(loadWorkbenchPaymentRecovery('42', storage)).toEqual(recovery);
  });

  it('reuses the exact durable idempotency key for the same account and quote', () => {
    const storage = memoryStorage();
    const input = {
      accountId: '42',
      repository: 'open-covenant/covenant',
      issueUrl: quote.issueUrl,
      quote,
    };

    const first = prepareWorkbenchPaymentRecovery(input, storage);
    saveWorkbenchPaymentRecovery({ ...first, phase: 'unpaid' }, storage);
    const retried = prepareWorkbenchPaymentRecovery(input, storage);

    expect(retried.phase).toBe('prepared');
    expect(retried.idempotencyKey).toBe(first.idempotencyKey);
  });

  it('blocks payment preparation when storage cannot be read back', () => {
    const values = new Map<string, string>();
    const storage = {
      getItem: () => null,
      setItem: (key: string, value: string) => values.set(key, value),
      removeItem: (key: string) => values.delete(key),
    };

    expect(() =>
      prepareWorkbenchPaymentRecovery(
        {
          accountId: '42',
          repository: 'open-covenant/covenant',
          issueUrl: quote.issueUrl,
          quote,
        },
        storage,
      ),
    ).toThrow(PaymentRecoveryStorageError);
  });

  it('blocks a second account from replacing an unresolved payment record', () => {
    const storage = memoryStorage();
    prepareWorkbenchPaymentRecovery(
      {
        accountId: '42',
        repository: 'open-covenant/covenant',
        issueUrl: quote.issueUrl,
        quote,
      },
      storage,
    );

    expect(() =>
      prepareWorkbenchPaymentRecovery(
        {
          accountId: '7',
          repository: 'open-covenant/covenant',
          issueUrl: quote.issueUrl,
          quote,
        },
        storage,
      ),
    ).toThrow('different GitHub account');
  });
});

describe('paymentAccountId', () => {
  it('uses the immutable GitHub id rather than the mutable login', () => {
    expect(paymentAccountId({ account: { githubId: '42', githubLogin: 'maintainer' } })).toBe('42');
  });

  it('rejects account data without a stable GitHub id', () => {
    expect(() => paymentAccountId({ account: { githubLogin: 'maintainer' } })).toThrow(
      'stable GitHub identity',
    );
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
