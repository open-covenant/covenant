import { createHash, createPrivateKey, createPublicKey, verify } from 'node:crypto';
import { describe, expect, it, vi } from 'vitest';
import {
  assertRefundCapacity,
  PolicyRequestError,
  PolicySignerClient,
  type RepositoryAdmissionBinding,
} from './policy-client.js';

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
const commitment = {
  repository: 'example/project',
  issueNumber: 17,
  baseRef: 'main',
  baseSha: 'd'.repeat(40),
  repositoryAuthorizedAt: '2026-08-21T23:00:00.000Z',
  authorizationEvidenceHash: 'e'.repeat(64),
};
const liabilityAdmission = {
  id: '44444444-4444-4444-8444-444444444444',
  quoteId: '33333333-3333-4333-8333-333333333333',
  repository: commitment.repository,
  issueNumber: commitment.issueNumber,
  baseRef: commitment.baseRef,
  baseSha: commitment.baseSha,
  reservationKeyHash: 'b'.repeat(64),
  paymentAuthorizationHash: 'c'.repeat(64),
  verifierAppId: '222',
  installationId: 777,
  repositorySelection: 'selected' as const,
  permissions: {
    checks: 'read' as const,
    contents: 'read' as const,
    issues: 'read' as const,
    metadata: 'read' as const,
    pull_requests: 'read' as const,
    statuses: 'read' as const,
  },
  tokenRepositories: 1 as const,
  tokenExpiresAt: '2026-08-22T01:00:00.000Z',
  admittedAt: '2026-08-22T00:00:00.000Z',
  evidenceHash: 'f'.repeat(64),
};

describe('PolicySignerClient', () => {
  it('requires exact-repository evidence from a distinct verifier App', async () => {
    const evidence = {
      ready: true,
      repository: 'example/project',
      verifierAppId: '222',
      installationId: 777,
      repositorySelection: 'selected',
      permissions: {
        checks: 'read',
        contents: 'read',
        issues: 'read',
        metadata: 'read',
        pull_requests: 'read',
        statuses: 'read',
      },
      tokenRepositories: 1,
      tokenExpiresAt: '2026-08-22T01:00:00.000Z',
    };
    const request = vi.fn(async () => Response.json(evidence));
    const client = new PolicySignerClient(
      {
        policySignerUrl: 'http://signer',
        policySignerToken: 'token',
        jobAuthoritySeed: authoritySeedBase64,
        githubAppId: '111',
      },
      request as typeof fetch,
      60_000,
      () => new Date('2026-08-22T00:00:00.000Z'),
    );

    await expect(client.assertRepositoryReady('Example/Project')).resolves.toEqual(evidence);
    expect(request).toHaveBeenCalledWith(
      'http://signer/v1/readiness/repository',
      expect.objectContaining({
        method: 'POST',
        body: JSON.stringify({ repository: 'example/project' }),
        headers: expect.objectContaining({ authorization: 'Bearer token' }),
      }),
    );

    const sharedApp = new PolicySignerClient(
      {
        policySignerUrl: 'http://signer',
        policySignerToken: 'token',
        jobAuthoritySeed: authoritySeedBase64,
        githubAppId: '222',
      },
      (async () => Response.json(evidence)) as typeof fetch,
      60_000,
      () => new Date('2026-08-22T00:00:00.000Z'),
    );
    await expect(sharedApp.assertRepositoryReady('example/project')).rejects.toThrow(
      'must be distinct',
    );
  });

  it('rejects verifier evidence for a different repository or expanded scope', async () => {
    const client = (body: object) =>
      new PolicySignerClient(
        {
          policySignerUrl: 'http://signer',
          policySignerToken: 'token',
          jobAuthoritySeed: authoritySeedBase64,
          githubAppId: '111',
        },
        (async () => Response.json(body)) as typeof fetch,
        60_000,
        () => new Date('2026-08-22T00:00:00.000Z'),
      );
    const evidence = {
      ready: true,
      repository: 'attacker/project',
      verifierAppId: '222',
      installationId: 777,
      repositorySelection: 'selected',
      permissions: {
        checks: 'read',
        contents: 'read',
        issues: 'read',
        metadata: 'read',
        pull_requests: 'read',
        statuses: 'read',
      },
      tokenRepositories: 1,
      tokenExpiresAt: '2026-08-22T01:00:00.000Z',
    };
    await expect(client(evidence).assertRepositoryReady('example/project')).rejects.toThrow(
      'different repository',
    );
    await expect(
      client({
        ...evidence,
        repository: 'example/project',
        permissions: { ...evidence.permissions, actions: 'read' },
      }).assertRepositoryReady('example/project'),
    ).rejects.toThrow();
  });

  it('binds durable admission to the reservation and validates historical evidence', async () => {
    const paymentAuthorization = Buffer.from('signed payment authorization').toString('base64');
    const binding: RepositoryAdmissionBinding = {
      quoteId: '33333333-3333-4333-8333-333333333333',
      repository: 'example/project',
      issueNumber: 17,
      baseRef: 'main',
      baseSha: 'a'.repeat(40),
      reservationKeyHash: 'b'.repeat(64),
      paymentAuthorizationHash: createHash('sha256').update(paymentAuthorization).digest('hex'),
    };
    const receipt = {
      id: '44444444-4444-4444-8444-444444444444',
      ...binding,
      verifierAppId: '222',
      installationId: 777,
      repositorySelection: 'selected',
      permissions: {
        checks: 'read',
        contents: 'read',
        issues: 'read',
        metadata: 'read',
        pull_requests: 'read',
        statuses: 'read',
      },
      tokenRepositories: 1,
      tokenExpiresAt: '2026-08-22T01:00:00.000Z',
      admittedAt: '2026-08-22T00:00:00.000Z',
      evidenceHash: 'd'.repeat(64),
    };
    const createRequest = vi.fn(async () => Response.json(receipt));
    const client = new PolicySignerClient(
      {
        policySignerUrl: 'http://signer',
        policySignerToken: 'token',
        jobAuthoritySeed: authoritySeedBase64,
        githubAppId: '111',
      },
      createRequest as typeof fetch,
      60_000,
      () => new Date('2026-08-22T00:30:00.000Z'),
    );

    await expect(client.createRepositoryAdmission(binding, paymentAuthorization)).resolves.toEqual(
      receipt,
    );
    const { paymentAuthorizationHash: _, ...requestBinding } = binding;
    expect(createRequest).toHaveBeenCalledWith(
      'http://signer/v1/repository-admissions',
      expect.objectContaining({
        method: 'POST',
        headers: expect.objectContaining({
          'idempotency-key': `mizuki-repository-admission-${binding.quoteId}`,
        }),
        body: JSON.stringify({ ...requestBinding, paymentAuthorization }),
      }),
    );

    const validateRequest = vi.fn(async () => Response.json(receipt));
    const restarted = new PolicySignerClient(
      {
        policySignerUrl: 'http://signer',
        policySignerToken: 'token',
        jobAuthoritySeed: authoritySeedBase64,
        githubAppId: '111',
      },
      validateRequest as typeof fetch,
      60_000,
      () => new Date('2026-08-23T00:00:00.000Z'),
    );
    await expect(restarted.validateRepositoryAdmission(receipt, binding)).resolves.toEqual(receipt);
    expect(validateRequest).toHaveBeenCalledWith(
      `http://signer/v1/repository-admissions/${receipt.id}/validate`,
      expect.objectContaining({
        body: JSON.stringify({ ...binding, evidenceHash: receipt.evidenceHash }),
      }),
    );
    await expect(
      restarted.validateRepositoryAdmission(
        { ...receipt, paymentAuthorizationHash: 'e'.repeat(64) },
        binding,
      ),
    ).rejects.toThrow('does not match');

    await expect(
      client.createRepositoryAdmission(binding, `${paymentAuthorization}A`),
    ).rejects.toThrow('does not match');
    expect(createRequest).toHaveBeenCalledOnce();
  });

  it('requests signer-side reconciliation only for the authorization bound to the admission', async () => {
    const paymentAuthorization = Buffer.from('signed payment authorization').toString('base64');
    const receipt = {
      id: '44444444-4444-4444-8444-444444444444',
      quoteId: '33333333-3333-4333-8333-333333333333',
      repository: 'example/project',
      issueNumber: 17,
      baseRef: 'main',
      baseSha: 'a'.repeat(40),
      reservationKeyHash: 'b'.repeat(64),
      paymentAuthorizationHash: createHash('sha256').update(paymentAuthorization).digest('hex'),
      verifierAppId: '222',
      installationId: 777,
      repositorySelection: 'selected' as const,
      permissions: {
        checks: 'read' as const,
        contents: 'read' as const,
        issues: 'read' as const,
        metadata: 'read' as const,
        pull_requests: 'read' as const,
        statuses: 'read' as const,
      },
      tokenRepositories: 1 as const,
      tokenExpiresAt: '2026-08-22T01:00:00.000Z',
      admittedAt: '2026-08-22T00:00:00.000Z',
      evidenceHash: 'd'.repeat(64),
    };
    const evidence = {
      signature: '6'.repeat(64),
      payer: '4'.repeat(32),
      recipient: '2'.repeat(32),
      mint: 'EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v',
      rawAmount: '2000000',
      decimals: 6,
      finalized: true,
      succeeded: true,
      slot: 42,
      blockTimeUnixSeconds: 1_787_356_800,
    };
    const request = vi.fn(async () => Response.json(evidence));
    const signer = new PolicySignerClient(
      {
        policySignerUrl: 'http://signer',
        policySignerToken: 'token',
        jobAuthoritySeed: authoritySeedBase64,
        githubAppId: '111',
      },
      request as typeof fetch,
    );

    await expect(signer.reconcileRepositorySettlement(receipt)).resolves.toEqual(evidence);
    expect(request).toHaveBeenCalledWith(
      `http://signer/v1/repository-admissions/${receipt.id}/settlements/reconcile`,
      expect.objectContaining({
        body: JSON.stringify({ evidenceHash: receipt.evidenceHash }),
      }),
    );
    expect(request).toHaveBeenCalledOnce();
  });

  it('registers liability and sends distinct signed refund authorizations', async () => {
    const liability = {
      id: '22222222-2222-4222-8222-222222222222',
      jobId: 'job-1',
      repositoryAdmissionId: liabilityAdmission.id,
      settlementSignature: 'settlement-signature',
      ...commitment,
      reviewedHeadSha: null,
      reviewedBaseSha: null,
      reviewedBaseRef: null,
      reviewedDiffHash: null,
      deliveryBoundAt: null,
      deliveryBindingHash: null,
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
    await expect(
      client.registerRefundLiability(
        'job-1',
        'settlement-signature',
        commitment,
        liabilityAdmission,
      ),
    ).resolves.toMatchObject({ jobId: 'job-1' });
    await expect(client.refund('job-1', 'settlement-signature')).resolves.toMatchObject({
      status: 'finalized',
    });
    expect(request).toHaveBeenCalledTimes(2);
    const registration = JSON.parse(String(request.mock.calls[0]![1]?.body));
    const execution = JSON.parse(String(request.mock.calls[1]![1]?.body));
    expectSignedAuthorization(registration, 'register');
    expectSignedAuthorization(execution, 'execute');
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

  it('signs and submits the immutable delivery binding', async () => {
    const binding = {
      jobId: 'job-1',
      settlementSignature: 'settlement-signature',
      reviewedHeadSha: 'a'.repeat(40),
      reviewedBaseSha: 'd'.repeat(40),
      reviewedBaseRef: 'main',
      reviewedDiffHash: 'f'.repeat(64),
    };
    const request = vi.fn(async () =>
      Response.json({
        id: '22222222-2222-4222-8222-222222222222',
        repositoryAdmissionId: liabilityAdmission.id,
        ...binding,
        ...commitment,
        deliveryBoundAt: '2026-08-22T00:00:00.000Z',
        deliveryBindingHash: 'b'.repeat(64),
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
      }),
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

    await client.bindRefundLiabilityDelivery('22222222-2222-4222-8222-222222222222', binding);
    const [url, init] = request.mock.calls[0]!;
    expect(url).toBe(
      'http://signer/v1/refund-liabilities/22222222-2222-4222-8222-222222222222/delivery-bindings',
    );
    expect(init?.headers).toMatchObject({
      'idempotency-key': 'mizuki-refund-liability-delivery-job-1',
    });
    const body = JSON.parse(String(init?.body));
    const message = [
      'Mizuki refund liability delivery binding',
      'Version: 1',
      'Job: job-1',
      'Settlement: settlement-signature',
      `Reviewed Head: ${'a'.repeat(40)}`,
      `Reviewed Base SHA: ${'d'.repeat(40)}`,
      'Reviewed Base Ref: main',
      `Reviewed Diff: ${'f'.repeat(64)}`,
      'Expires At: 2026-08-22T00:05:00.000Z',
    ].join('\n');
    expect(
      verify(
        null,
        Buffer.from(message),
        createPublicKey(authorityPrivateKey),
        Buffer.from(body.authorizationSignature, 'base64'),
      ),
    ).toBe(true);
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

  it('binds escrow release authorization to the reviewed head and diff', async () => {
    const request = vi.fn(async () =>
      Response.json({
        ...finalized,
        kind: 'escrow_release',
        asset: 'SOL',
        recipient: 'claimant',
      }),
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
    const evidence = {
      repository: 'example/project',
      issueNumber: 17,
      pullRequestNumber: 23,
      mergeCommitSha: 'f'.repeat(40),
      reviewedHeadSha: 'a'.repeat(40),
      reviewedBaseSha: 'd'.repeat(40),
      reviewedBaseRef: 'main',
      reviewedDiffHash: 'b'.repeat(64),
      reviewReceiptId: '77777777-7777-4777-8777-777777777777',
      reviewReceiptHash: 'e'.repeat(64),
      reviewModel: 'independent-reviewer',
      reviewRoute: 'marketplace' as const,
      reviewedAt: '2026-08-21T23:59:00.000Z',
    };

    await expect(
      client.releaseEscrow('11111111-1111-4111-8111-111111111111', evidence),
    ).resolves.toMatchObject({ kind: 'escrow_release', status: 'finalized' });
    expect(request).toHaveBeenCalledWith(
      'http://signer/v1/escrows/11111111-1111-4111-8111-111111111111/release',
      expect.objectContaining({
        headers: expect.objectContaining({
          'idempotency-key': 'mizuki-escrow-release-11111111-1111-4111-8111-111111111111',
        }),
        body: expect.any(String),
      }),
    );
    const body = JSON.parse(String(request.mock.calls[0]![1]?.body));
    expect(body).toMatchObject({
      ...evidence,
      authorizationExpiresAt: '2026-08-22T00:05:00.000Z',
      authorizationSignature: expect.any(String),
    });
    const message = [
      'Mizuki escrow release authorization',
      'Version: 1',
      'Escrow: 11111111-1111-4111-8111-111111111111',
      'Repository: example/project',
      'Issue: 17',
      'Pull Request: 23',
      `Merge Commit: ${'f'.repeat(40)}`,
      `Reviewed Head: ${'a'.repeat(40)}`,
      `Reviewed Base SHA: ${'d'.repeat(40)}`,
      'Reviewed Base Ref: main',
      `Reviewed Diff: ${'b'.repeat(64)}`,
      'Review Receipt: 77777777-7777-4777-8777-777777777777',
      `Review Receipt Hash: ${'e'.repeat(64)}`,
      'Review Model: independent-reviewer',
      'Review Route: marketplace',
      'Reviewed At: 2026-08-21T23:59:00.000Z',
      'Expires At: 2026-08-22T00:05:00.000Z',
    ].join('\n');
    expect(
      verify(
        null,
        Buffer.from(message),
        createPublicKey(authorityPrivateKey),
        Buffer.from(body.authorizationSignature, 'base64'),
      ),
    ).toBe(true);
  });

  it('signs a normalized merged-PR liability discharge', async () => {
    const discharged = {
      id: '22222222-2222-4222-8222-222222222222',
      jobId: 'job-1',
      repositoryAdmissionId: liabilityAdmission.id,
      settlementSignature: 'settlement-signature',
      ...commitment,
      reviewedHeadSha: 'a'.repeat(40),
      reviewedBaseSha: 'd'.repeat(40),
      reviewedBaseRef: 'main',
      reviewedDiffHash: 'f'.repeat(64),
      deliveryBoundAt: '2026-08-22T00:00:30.000Z',
      deliveryBindingHash: 'b'.repeat(64),
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
      issueNumber: 17,
      pullRequestNumber: 17,
      deliveredCommitSha: 'a'.repeat(40),
      reviewedHeadSha: 'a'.repeat(40),
      reviewedBaseSha: 'd'.repeat(40),
      reviewedBaseRef: 'main',
      reviewedDiffHash: 'f'.repeat(64),
    });
    const body = JSON.parse(String(request.mock.calls[0]![1]?.body));
    expect(body.repository).toBe('example/project');
    const message = [
      'Mizuki refund liability discharge authorization',
      'Version: 2',
      'Job: job-1',
      'Settlement: settlement-signature',
      'Repository: example/project',
      'Issue: 17',
      'Pull Request: 17',
      `Delivered Commit: ${'a'.repeat(40)}`,
      `Reviewed Head: ${'a'.repeat(40)}`,
      `Reviewed Base SHA: ${'d'.repeat(40)}`,
      'Reviewed Base Ref: main',
      `Reviewed Diff: ${'f'.repeat(64)}`,
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
    ).toThrow('configured escrow return recipient');
  });
});

function expectSignedAuthorization(
  body: Record<string, string>,
  action: 'register' | 'execute',
): void {
  expect(body).toMatchObject({
    jobId: 'job-1',
    settlementSignature: 'settlement-signature',
    ...(action === 'register'
      ? {
          repositoryAdmissionId: liabilityAdmission.id,
          repositoryAdmissionEvidenceHash: liabilityAdmission.evidenceHash,
        }
      : {}),
    authorizationExpiresAt: '2026-08-22T00:05:00.000Z',
  });
  const message = [
    action === 'register'
      ? 'Mizuki refund liability registration'
      : 'Mizuki refund execution authorization',
    `Version: ${action === 'register' ? 3 : 1}`,
    'Job: job-1',
    'Settlement: settlement-signature',
    ...(action === 'register'
      ? [
          `Repository Admission: ${liabilityAdmission.id}`,
          `Repository Admission Evidence: ${liabilityAdmission.evidenceHash}`,
          'Repository: example/project',
          'Issue: 17',
          'Base Ref: main',
          `Base SHA: ${'d'.repeat(40)}`,
          'Repository Authorized At: 2026-08-21T23:00:00.000Z',
          `Authorization Evidence: ${'e'.repeat(64)}`,
        ]
      : []),
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
