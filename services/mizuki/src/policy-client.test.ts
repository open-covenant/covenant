import { createPrivateKey, createPublicKey, verify } from 'node:crypto';
import { describe, expect, it, vi } from 'vitest';
import { assertRefundCapacity, PolicyRequestError, PolicySignerClient } from './policy-client.js';

const authoritySeed = Buffer.alloc(32, 7);
const authoritySeedBase64 = authoritySeed.toString('base64');
const authorityPrivateKey = createPrivateKey({
  key: Buffer.concat([Buffer.from('302e020100300506032b657004220420', 'hex'), authoritySeed]),
  format: 'der',
  type: 'pkcs8',
});

const finalized = {
  id: '11111111-1111-4111-8111-111111111111',
  kind: 'refund',
  status: 'finalized',
  amountUsdCents: 200,
  amountAtomic: null,
  asset: 'USDC',
  recipient: 'payer',
  transactionSignature: 'refund-signature',
  error: null,
  createdAt: '2026-08-22T00:00:00Z',
  updatedAt: '2026-08-22T00:00:01Z',
};

describe('PolicySignerClient', () => {
  it('registers liability and sends distinct signed refund authorizations', async () => {
    const liability = {
      id: '22222222-2222-4222-8222-222222222222',
      jobId: 'job-1',
      settlementSignature: 'settlement-signature',
      payer: 'payer',
      mint: 'mint',
      rawAmount: '2000000',
      decimals: 6,
      amountUsdCents: 200,
      settlementSlot: 12,
      settlementBlockTimeUnixSeconds: 1_787_356_800,
      createdAt: '2026-08-22T00:00:00.000Z',
      dischargedAt: null,
      dischargeEvidenceHash: null,
    };
    const request = vi.fn(async (url: string | URL | Request) =>
      Response.json(String(url).endsWith('/v1/refund-liabilities') ? liability : finalized),
    );
    const client = new PolicySignerClient(
      {
        policySignerUrl: 'http://signer',
        policySignerToken: 'token',
        jobAuthoritySeed: authoritySeedBase64,
      },
      request as typeof fetch,
      60_000,
      () => new Date('2026-08-22T00:00:00.000Z'),
    );
    await expect(client.refund('job-1', 'settlement-signature')).resolves.toMatchObject({
      status: 'finalized',
    });
    expect(request).toHaveBeenCalledTimes(2);
    const registration = JSON.parse(String(request.mock.calls[0]![1]?.body));
    const execution = JSON.parse(String(request.mock.calls[1]![1]?.body));
    expectSignedAuthorization(registration, 'Mizuki refund liability registration');
    expectSignedAuthorization(execution, 'Mizuki refund execution authorization');
  });

  it('rejects malformed signer responses', async () => {
    const client = new PolicySignerClient(
      {
        policySignerUrl: 'http://signer',
        policySignerToken: 'token',
        jobAuthoritySeed: authoritySeedBase64,
      },
      (async () => Response.json({ status: 'finalized' })) as typeof fetch,
    );
    await expect(client.refund('job-1', 'settlement-signature')).rejects.toThrow();
  });

  it('classifies retryable signer failures', async () => {
    const client = new PolicySignerClient(
      {
        policySignerUrl: 'http://signer',
        policySignerToken: 'token',
        jobAuthoritySeed: authoritySeedBase64,
      },
      (async () => new Response('temporarily unavailable', { status: 503 })) as typeof fetch,
    );

    const failure = await client.refund('job-1', 'settlement-signature').catch((cause) => cause);
    expect(failure).toBeInstanceOf(PolicyRequestError);
    expect(failure).toMatchObject({
      code: 'policy_request_failed',
      status: 503,
      retryable: true,
    });
  });

  it('signs a normalized merged-PR liability discharge', async () => {
    const discharged = {
      id: '22222222-2222-4222-8222-222222222222',
      jobId: 'job-1',
      settlementSignature: 'settlement-signature',
      payer: 'payer',
      mint: 'mint',
      rawAmount: '2000000',
      decimals: 6,
      amountUsdCents: 200,
      settlementSlot: 12,
      settlementBlockTimeUnixSeconds: 1_787_356_800,
      createdAt: '2026-08-22T00:00:00.000Z',
      dischargedAt: '2026-08-22T00:01:00.000Z',
      dischargeEvidenceHash: 'a'.repeat(64),
    };
    const request = vi.fn(async () => Response.json(discharged));
    const client = new PolicySignerClient(
      {
        policySignerUrl: 'http://signer',
        policySignerToken: 'token',
        jobAuthoritySeed: authoritySeedBase64,
      },
      request as typeof fetch,
      60_000,
      () => new Date('2026-08-22T00:00:00.000Z'),
    );
    await client.dischargeRefundLiability(discharged.id, {
      jobId: 'job-1',
      settlementSignature: 'settlement-signature',
      repository: 'Example/Project',
      pullRequestNumber: 17,
    });
    const body = JSON.parse(String(request.mock.calls[0]![1]?.body));
    expect(body.repository).toBe('example/project');
    const message = [
      'Mizuki refund liability discharge authorization',
      'Version: 1',
      'Job: job-1',
      'Settlement: settlement-signature',
      'Repository: example/project',
      'Pull Request: 17',
      'Expires At: 2026-08-22T00:05:00.000Z',
    ].join('\n');
    expect(
      verify(
        null,
        Buffer.from(message, 'utf8'),
        createPublicKey(authorityPrivateKey),
        Buffer.from(body.authorizationSignature, 'base64'),
      ),
    ).toBe(true);
  });

  it('requires exact signer identity and enough finalized refund capacity', () => {
    const readiness = {
      healthy: true,
      refundTreasury: 'treasury',
      refundMint: 'mint',
      refundDecimals: 6,
      finalizedBalanceRaw: '12000000',
      pendingRefundRaw: '1000000',
      treasuryAvailableRefundRaw: '11000000',
      remainingRefundLimitUsdCents: 1_100,
      availableRefundRaw: '11000000',
      escrowAuthority: 'escrow-authority',
      finalizedEscrowBalanceLamports: '2000000000',
      availableEscrowReserveLamports: '1900000000',
    };
    expect(() =>
      assertRefundCapacity({
        readiness,
        treasury: 'treasury',
        mint: 'mint',
        decimals: 6,
        escrowAuthority: 'escrow-authority',
        unfinishedLiabilityRaw: 9_000_000n,
        proposedPaymentRaw: 2_000_000n,
      }),
    ).not.toThrow();
    expect(() =>
      assertRefundCapacity({
        readiness,
        treasury: 'treasury',
        mint: 'mint',
        decimals: 6,
        unfinishedLiabilityRaw: 9_000_001n,
        proposedPaymentRaw: 2_000_000n,
      }),
    ).toThrow('cannot cover');
    expect(() =>
      assertRefundCapacity({
        readiness: { ...readiness, refundTreasury: 'other' },
        treasury: 'treasury',
        mint: 'mint',
        decimals: 6,
        unfinishedLiabilityRaw: 0n,
        proposedPaymentRaw: 1n,
      }),
    ).toThrow('does not match');
    expect(() =>
      assertRefundCapacity({
        readiness,
        treasury: 'treasury',
        mint: 'mint',
        decimals: 6,
        escrowAuthority: 'another-capability-wallet',
        unfinishedLiabilityRaw: 0n,
        proposedPaymentRaw: 1n,
      }),
    ).toThrow('capability payout wallet');
  });
});

function expectSignedAuthorization(body: Record<string, string>, title: string): void {
  expect(body).toMatchObject({
    jobId: 'job-1',
    settlementSignature: 'settlement-signature',
    authorizationExpiresAt: '2026-08-22T00:05:00.000Z',
  });
  const message = [
    title,
    'Version: 1',
    'Job: job-1',
    'Settlement: settlement-signature',
    'Expires At: 2026-08-22T00:05:00.000Z',
  ].join('\n');
  expect(
    verify(
      null,
      Buffer.from(message, 'utf8'),
      createPublicKey(authorityPrivateKey),
      Buffer.from(body.authorizationSignature!, 'base64'),
    ),
  ).toBe(true);
}
