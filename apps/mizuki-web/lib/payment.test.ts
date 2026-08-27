import { describe, expect, it, vi } from 'vitest';
import {
  checkQuotePaymentStatus,
  clearWorkbenchPaymentRecovery,
  loadWorkbenchPaymentRecovery,
  paymentAccountId,
  paymentRetryAllowed,
  PaymentStatusError,
  normalizePaymentAttempt,
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
  it('does not reopen the wallet from stale retry-safe state after local authorization', () => {
    expect(paymentRetryAllowed({ walletAuthorized: true }, { retrySafe: true })).toBe(false);
    expect(paymentRetryAllowed({ walletAuthorized: false }, { retrySafe: true })).toBe(true);
  });

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
      attemptId: 'attempt-11111111',
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
        attemptId: 'attempt-11111111',
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
      attemptId: 'attempt-11111111',
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

  it('caches the server-owned attempt and idempotency key without replacing either', () => {
    const storage = memoryStorage();
    const input = {
      accountId: '42',
      attemptId: 'attempt-11111111',
      idempotencyKey: 'aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa',
      repository: 'open-covenant/covenant',
      issueUrl: quote.issueUrl,
      quote,
    };

    const first = prepareWorkbenchPaymentRecovery(input, storage);
    saveWorkbenchPaymentRecovery({ ...first, phase: 'unpaid' }, storage);
    const retried = prepareWorkbenchPaymentRecovery(input, storage);

    expect(retried.phase).toBe('prepared');
    expect(retried.idempotencyKey).toBe(first.idempotencyKey);
    expect(retried.attemptId).toBe(first.attemptId);
  });

  it('does not let unavailable browser storage block a server-owned payment attempt', () => {
    const values = new Map<string, string>();
    const storage = {
      getItem: () => null,
      setItem: (key: string, value: string) => values.set(key, value),
      removeItem: (key: string) => values.delete(key),
    };

    const recovery = prepareWorkbenchPaymentRecovery(
      {
        accountId: '42',
        attemptId: 'attempt-11111111',
        idempotencyKey: 'aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa',
        repository: 'open-covenant/covenant',
        issueUrl: quote.issueUrl,
        quote,
      },
      storage,
    );

    expect(recovery.attemptId).toBe('attempt-11111111');
    expect(loadWorkbenchPaymentRecovery('42', storage)).toBeNull();
  });

  it('accepts a canonical recovery quote before a new payment challenge is attached', () => {
    const storage = memoryStorage();
    const canonicalQuote = { ...quote, payment: undefined };
    const recovery = prepareWorkbenchPaymentRecovery(
      {
        accountId: '42',
        attemptId: 'attempt-11111111',
        idempotencyKey: 'aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa',
        repository: 'open-covenant/covenant',
        issueUrl: canonicalQuote.issueUrl,
        quote: canonicalQuote,
      },
      storage,
    );

    expect(loadWorkbenchPaymentRecovery('42', storage)).toEqual(recovery);
  });

  it('allows the signed-in account to replace the optional cache', () => {
    const storage = memoryStorage();
    prepareWorkbenchPaymentRecovery(
      {
        accountId: '42',
        attemptId: 'attempt-11111111',
        idempotencyKey: 'aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa',
        repository: 'open-covenant/covenant',
        issueUrl: quote.issueUrl,
        quote,
      },
      storage,
    );

    prepareWorkbenchPaymentRecovery(
      {
        accountId: '7',
        attemptId: 'attempt-22222222',
        idempotencyKey: 'bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb',
        repository: 'open-covenant/covenant',
        issueUrl: quote.issueUrl,
        quote,
      },
      storage,
    );

    expect(loadWorkbenchPaymentRecovery('42', storage)).toBeNull();
    expect(loadWorkbenchPaymentRecovery('7', storage)?.attemptId).toBe('attempt-22222222');
  });
});

describe('server-owned payment attempts', () => {
  it('uses canonical top-level status and reserved job data', () => {
    expect(
      normalizePaymentAttempt(
        {
          attempt: {
            id: 'attempt-11111111',
            quoteId: quote.id,
            idempotencyKey: 'aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa',
            stage: 'submitting',
            retrySafe: false,
          },
          paymentStatus: 'job_reserved',
          retrySafe: false,
          job: { id: 'job-11111111', state: 'settlement_pending' },
          requestId: 'request-11111111',
          buildId: 'build-11111111',
        },
        quote.id,
      ),
    ).toMatchObject({
      id: 'attempt-11111111',
      paymentStatus: 'job_reserved',
      retrySafe: false,
      job: { id: 'job-11111111' },
      requestId: 'request-11111111',
      buildId: 'build-11111111',
    });
  });

  it('rejects a status bound to a different quote', () => {
    expect(() =>
      normalizePaymentAttempt(
        {
          id: 'attempt-11111111',
          quoteId: '22222222-2222-4222-8222-222222222222',
          idempotencyKey: 'aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa',
          stage: 'created',
          paymentStatus: 'created',
          retrySafe: true,
        },
        quote.id,
      ),
    ).toThrow('did not match');
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
