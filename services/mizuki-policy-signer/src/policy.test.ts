import { createHash, createPrivateKey, sign } from 'node:crypto';
import { Keypair, SystemProgram, TransactionMessage, VersionedTransaction } from '@solana/web3.js';
import { describe, expect, it } from 'vitest';
import {
  authorizedSettlementTransaction,
  FixedUsdPriceOracle,
  MockChainGateway,
  type UsdPriceOracle,
} from './chain.js';
import type {
  BindRefundLiabilityDeliveryRequest,
  DischargeRefundLiabilityRequest,
  RefundRequest,
  RegisterRefundLiabilityRequest,
  RepositoryAdmissionRequest,
  SettlementFacts,
} from './domain.js';
import {
  operationView,
  PAYMENT_AUTHORIZATION_MAX_BYTES,
  PolicyError,
  repositoryAdmissionRequestSchema,
  reconcileRepositorySettlementRequestSchema,
  refundAuthorizationMessage,
  refundDeliveryBindingAuthorizationMessage,
  refundDischargeAuthorizationMessage,
  requestHash,
} from './domain.js';
import { MockMergeVerifier } from './github.js';
import { SignerMetrics } from './metrics.js';
import { PolicyService } from './policy.js';
import { InMemoryOperationStore } from './store.js';

const TREASURY = '2'.repeat(32);
const MINT = '3'.repeat(32);
const PAYER = '4'.repeat(32);
const CLAIMANT_KEYPAIR = Keypair.generate();
const CLAIMANT = CLAIMANT_KEYPAIR.publicKey.toBase58();
const JOB_AUTHORITY = Keypair.generate();
const DEFAULT_ADMISSION_ID = '99999999-9999-4999-8999-999999999999';
let defaultAdmissionEvidenceHash = '';

function releaseRequest(pullRequestNumber = 23) {
  return {
    pullRequestNumber,
    reviewedHeadSha: 'b'.repeat(40),
    reviewedDiffHash: 'c'.repeat(64),
  };
}

function fixture(
  options: {
    dailyLimit?: number;
    refundDailyLimit?: number;
    escrowDailyLimit?: number;
    operationLimit?: number;
    maxEscrowLamports?: number;
    now?: () => Date;
    prices?: UsdPriceOracle;
    jobAuthorityPublicKey?: string;
    refundTreasury?: string;
    escrowAuthority?: string;
  } = {},
) {
  const store = new InMemoryOperationStore();
  const chain = new MockChainGateway();
  chain.now = () => (options.now ? options.now().getTime() : Date.now());
  const metrics = new SignerMetrics();
  const merges = new MockMergeVerifier();
  const policy = new PolicyService(
    {
      refundTreasury: options.refundTreasury ?? TREASURY,
      escrowAuthority: options.escrowAuthority ?? '5'.repeat(32),
      refundMint: MINT,
      refundDecimals: 6,
      jobAuthorityPublicKey: options.jobAuthorityPublicKey ?? JOB_AUTHORITY.publicKey.toBase58(),
      refundAuthMaxTtlSeconds: 900,
      operationLimitUsdCents: options.operationLimit ?? 2_500,
      refundDailyLimitUsdCents: options.refundDailyLimit ?? options.dailyLimit ?? 10_000,
      escrowDailyLimitUsdCents: options.escrowDailyLimit ?? options.dailyLimit ?? 10_000,
      maxEscrowLamports: options.maxEscrowLamports ?? 1_000_000_000,
      solFeeReserveLamports: 1_000_000,
      bindChallengeTtlSeconds: 600,
      githubGrantTtlSeconds: 600,
      claimTtlSeconds: 172_800,
      leaseMs: 5_000,
    },
    store,
    chain,
    options.prices ?? new FixedUsdPriceOracle(100_000_000),
    merges,
    metrics,
    options.now,
  );
  const admittedAt = options.now?.() ?? new Date();
  const payment = paymentAuthorization(DEFAULT_ADMISSION_ID);
  const paymentAuthorizationHash = createHash('sha256').update(payment.header).digest('hex');
  const settlementIdentity = authorizedSettlementTransaction({
    wireTransaction: payment.wireTransaction,
    feePayer: payment.feePayer,
    rawAmount: '2000000',
    notBeforeUnixSeconds: 0,
  });
  const tokenExpiresAt = new Date(admittedAt.getTime() + 60 * 60_000);
  const identity = {
    quoteId: DEFAULT_ADMISSION_ID,
    repository: 'owner/repository',
    issueNumber: 17,
    baseRef: 'main',
    baseSha: 'd'.repeat(40),
    reservationKeyHash: '9'.repeat(64),
    paymentAuthorizationHash,
  };
  const binding = {
    settlementMessageHash: settlementIdentity.messageHash,
    settlementClientSignature: settlementIdentity.clientSignature,
    settlementFeePayer: settlementIdentity.feePayer,
    settlementRawAmount: '2000000',
    paymentWindowStartUnixSeconds: Math.floor(admittedAt.getTime() / 1_000) - 30,
    paymentWindowEndUnixSeconds: Math.floor(admittedAt.getTime() / 1_000) + 330,
  };
  defaultAdmissionEvidenceHash = requestHash({
    version: 1,
    ...identity,
    ...binding,
    verifierAppId: '12345',
    installationId: 777,
    repositorySelection: 'selected',
    permissions: {
      contents: 'read',
      issues: 'read',
      metadata: 'read',
      pull_requests: 'read',
    },
    tokenRepositories: 1,
    tokenExpiresAt: tokenExpiresAt.toISOString(),
    admittedAt: admittedAt.toISOString(),
  });
  void store.registerRepositoryAdmission({
    id: DEFAULT_ADMISSION_ID,
    idempotencyKey: 'default-admission',
    requestHash: requestHash(identity),
    ...identity,
    ...binding,
    verifierAppId: '12345',
    installationId: 777,
    repositorySelection: 'selected',
    permissions: {
      contents: 'read',
      issues: 'read',
      metadata: 'read',
      pull_requests: 'read',
    },
    tokenRepositories: 1,
    tokenExpiresAt,
    admittedAt,
    evidenceHash: defaultAdmissionEvidenceHash,
  });
  return { store, chain, metrics, merges, policy };
}

function settlement(signature = '6'.repeat(64), rawAmount = '2000000'): SettlementFacts {
  return {
    signature,
    payer: PAYER,
    recipient: TREASURY,
    mint: MINT,
    rawAmount,
    decimals: 6,
    finalized: true,
    succeeded: true,
    slot: 42,
    blockTimeUnixSeconds: Math.floor(Date.now() / 1_000),
  };
}

function paymentAuthorization(quoteId: string): {
  header: string;
  wireTransaction: string;
  feePayer: string;
} {
  const feePayer = Keypair.generate();
  const payer = Keypair.generate();
  const recipient = Keypair.generate();
  const message = new TransactionMessage({
    payerKey: feePayer.publicKey,
    recentBlockhash: Keypair.generate().publicKey.toBase58(),
    instructions: [
      SystemProgram.transfer({
        fromPubkey: payer.publicKey,
        toPubkey: recipient.publicKey,
        lamports: 1,
      }),
    ],
  }).compileToV0Message();
  const transaction = new VersionedTransaction(message);
  transaction.sign([payer]);
  const wireTransaction = Buffer.from(transaction.serialize()).toString('base64');
  const header = Buffer.from(
    JSON.stringify({
      x402Version: 2,
      resource: { url: `https://mizuki.example/v1/jobs?quote_id=${quoteId}` },
      accepted: {
        scheme: 'exact',
        network: 'solana:5eykt4UsFv8P8NJdTREpY1vzqKqZKvdp',
        asset: MINT,
        amount: '2000000',
        payTo: TREASURY,
        maxTimeoutSeconds: 300,
        extra: { feePayer: feePayer.publicKey.toBase58() },
      },
      payload: { transaction: wireTransaction },
    }),
  ).toString('base64');
  return { header, wireTransaction, feePayer: feePayer.publicKey.toBase58() };
}

describe('production readiness', () => {
  it('accepts exactly the payment authorization size supported by core recovery', () => {
    const request = (paymentAuthorization: string) => ({
      quoteId: '11111111-1111-4111-8111-111111111111',
      repository: 'owner/repository',
      issueNumber: 17,
      baseRef: 'main',
      baseSha: 'a'.repeat(40),
      reservationKeyHash: 'b'.repeat(64),
      paymentAuthorization,
    });
    expect(
      repositoryAdmissionRequestSchema.safeParse(
        request('A'.repeat(PAYMENT_AUTHORIZATION_MAX_BYTES)),
      ).success,
    ).toBe(true);
    expect(
      repositoryAdmissionRequestSchema.safeParse(
        request('A'.repeat(PAYMENT_AUTHORIZATION_MAX_BYTES + 1)),
      ).success,
    ).toBe(false);
  });

  it('preserves exact-repository admission across App removal and rejects rebinding', async () => {
    const observedAt = new Date();
    const { policy, merges } = fixture({ now: () => observedAt });
    const authorization = paymentAuthorization('11111111-1111-4111-8111-111111111111');
    const request: RepositoryAdmissionRequest = {
      quoteId: '11111111-1111-4111-8111-111111111111',
      repository: 'owner/repository',
      issueNumber: 17,
      baseRef: 'main',
      baseSha: 'a'.repeat(40),
      reservationKeyHash: 'b'.repeat(64),
      paymentAuthorization: authorization.header,
    };

    const first = await policy.createRepositoryAdmission(request, 'repository-admission-key');
    expect(first).toMatchObject({
      quoteId: request.quoteId,
      paymentAuthorizationHash: createHash('sha256').update(authorization.header).digest('hex'),
      verifierAppId: '12345',
      installationId: 777,
      tokenRepositories: 1,
      evidenceHash: expect.stringMatching(/^[a-f0-9]{64}$/),
    });
    expect(merges.readinessRequests).toEqual(['owner/repository']);

    merges.error = new PolicyError(
      'github_installation_missing',
      'Verifier App was removed',
      503,
      true,
    );
    const replay = await policy.createRepositoryAdmission(request, 'repository-admission-key');
    expect(replay.id).toBe(first.id);
    expect(merges.readinessRequests).toEqual(['owner/repository']);
    await expect(
      policy.validateRepositoryAdmission(first.id, {
        quoteId: request.quoteId,
        repository: request.repository,
        issueNumber: request.issueNumber,
        baseRef: request.baseRef,
        baseSha: request.baseSha,
        reservationKeyHash: request.reservationKeyHash,
        paymentAuthorizationHash: first.paymentAuthorizationHash,
        evidenceHash: first.evidenceHash,
      }),
    ).resolves.toEqual(first);
    await expect(
      policy.validateRepositoryAdmission(first.id, {
        quoteId: request.quoteId,
        repository: request.repository,
        issueNumber: request.issueNumber,
        baseRef: request.baseRef,
        baseSha: request.baseSha,
        reservationKeyHash: request.reservationKeyHash,
        paymentAuthorizationHash: 'd'.repeat(64),
        evidenceHash: first.evidenceHash,
      }),
    ).rejects.toMatchObject({ code: 'repository_admission_mismatch' });
    await expect(
      policy.createRepositoryAdmission(
        {
          ...request,
          quoteId: '22222222-2222-4222-8222-222222222222',
          paymentAuthorization: paymentAuthorization('22222222-2222-4222-8222-222222222222').header,
        },
        'second-repository-admission-key',
      ),
    ).rejects.toMatchObject({ code: 'github_installation_missing' });
  });

  it('reconciles only the exact payer-signed transaction bound to durable admission', async () => {
    const { policy, chain } = fixture();
    const quoteId = '11111111-1111-4111-8111-111111111111';
    const authorization = paymentAuthorization(quoteId);
    const request: RepositoryAdmissionRequest = {
      quoteId,
      repository: 'owner/repository',
      issueNumber: 17,
      baseRef: 'main',
      baseSha: 'a'.repeat(40),
      reservationKeyHash: 'b'.repeat(64),
      paymentAuthorization: authorization.header,
    };
    const admission = await policy.createRepositoryAdmission(request, 'reconcile-admission-key');
    const facts = settlement();
    chain.reconciledSettlements.set(admission.settlementMessageHash, facts);

    await expect(
      policy.reconcileRepositorySettlement(admission.id, {
        evidenceHash: admission.evidenceHash,
      }),
    ).resolves.toEqual(facts);
    expect(chain.settlementReconciliations).toEqual([
      {
        messageHash: admission.settlementMessageHash,
        clientSignature: admission.settlementClientSignature,
        feePayer: authorization.feePayer,
        rawAmount: '2000000',
        notBeforeUnixSeconds: Math.floor(admission.admittedAt.getTime() / 1_000) - 30,
        notAfterUnixSeconds: Math.floor(admission.admittedAt.getTime() / 1_000) + 330,
      },
    ]);
    await expect(
      policy.reconcileRepositorySettlement(admission.id, {
        evidenceHash: 'f'.repeat(64),
      }),
    ).rejects.toMatchObject({ code: 'repository_admission_mismatch' });
    expect(chain.settlementReconciliations).toHaveLength(1);
  });

  it('fails closed on settlement absence, provider disagreement, and amount mismatch', async () => {
    const { policy, chain } = fixture();
    const quoteId = '22222222-2222-4222-8222-222222222222';
    const authorization = paymentAuthorization(quoteId);
    const admission = await policy.createRepositoryAdmission(
      {
        quoteId,
        repository: 'owner/repository',
        issueNumber: 18,
        baseRef: 'main',
        baseSha: 'a'.repeat(40),
        reservationKeyHash: 'b'.repeat(64),
        paymentAuthorization: authorization.header,
      },
      'reconcile-failure-key',
    );
    const reconcile = () =>
      policy.reconcileRepositorySettlement(admission.id, {
        evidenceHash: admission.evidenceHash,
      });

    await expect(reconcile()).rejects.toMatchObject({ code: 'settlement_not_found' });

    chain.reconciliationError = new PolicyError(
      'rpc_inconsistent',
      'Independent RPC providers disagree',
      503,
      true,
    );
    await expect(reconcile()).rejects.toMatchObject({ code: 'rpc_inconsistent' });

    chain.reconciliationError = null;
    chain.reconciledSettlements.set(
      admission.settlementMessageHash,
      settlement('6'.repeat(64), '2000001'),
    );
    await expect(reconcile()).rejects.toMatchObject({ code: 'settlement_value_mismatch' });

    chain.reconciledSettlements.set(admission.settlementMessageHash, {
      ...settlement(),
      blockTimeUnixSeconds: admission.paymentWindowEndUnixSeconds + 1,
    });
    await expect(reconcile()).rejects.toMatchObject({ code: 'settlement_outside_payment_window' });
  });

  it('keeps the request authority separate from both custody authorities', () => {
    const authority = Keypair.generate().publicKey.toBase58();
    expect(() => fixture({ jobAuthorityPublicKey: authority, refundTreasury: authority })).toThrow(
      'Job authority must be distinct from both custody authorities',
    );
    expect(() => fixture({ jobAuthorityPublicKey: authority, escrowAuthority: authority })).toThrow(
      'Job authority must be distinct from both custody authorities',
    );
  });

  it('returns authenticated evidence for dual providers, pinned program, and custody', async () => {
    const observedAt = new Date('2026-08-23T12:00:00.000Z');
    const { policy } = fixture({ now: () => observedAt });

    await expect(policy.probeReadiness()).resolves.toMatchObject({
      healthy: true,
      observedAt: observedAt.toISOString(),
      checks: {
        database: true,
        rpcConsensus: true,
        priceConsensus: true,
        githubCredential: true,
        escrowProgram: true,
        refundCustody: true,
        bountyCustody: true,
      },
      chain: {
        rpcProviders: 2,
        escrowProgramId: '4'.repeat(32),
        escrowProgramDataSha256: 'a'.repeat(64),
        escrowProgramImmutable: true,
        refundTreasury: TREASURY,
        refundMint: MINT,
        refundDecimals: 6,
        refundRawAmount: '1000000000',
        escrowAuthority: '5'.repeat(32),
        escrowLamports: '100000000000',
        availableEscrowReserveLamports: '99994500000',
      },
      prices: { feedCount: 2, priceUsdMicros: 100_000_000 },
    });
  });

  it('fails readiness when the signer credential cannot be verified', async () => {
    const { merges, policy } = fixture();
    merges.error = new PolicyError(
      'github_credential_invalid',
      'GitHub signer credential is not valid',
      503,
    );

    await expect(policy.probeReadiness()).resolves.toMatchObject({
      healthy: false,
      checks: { githubCredential: false },
    });
    await expect(policy.readiness()).resolves.toMatchObject({
      healthy: false,
      availableRefundRaw: null,
      availableEscrowReserveLamports: null,
    });
  });

  it('rejects a price source that lacks two named observations', async () => {
    const observedAt = new Date();
    const prices: UsdPriceOracle = {
      solUsd: async () => ({ priceUsdMicros: 100_000_000, observedAt }),
    };
    const { policy } = fixture({ prices });

    await expect(policy.probeReadiness()).resolves.toMatchObject({
      healthy: false,
      checks: { priceConsensus: false },
      prices: null,
    });
  });
});

describe('refund policy', () => {
  it('requires action-bound authorization and a pre-registered job liability', async () => {
    const { chain, policy } = fixture();
    const facts = settlement();
    chain.settlements.set(facts.signature, facts);

    await expect(
      policy.refund(
        signedRefundRequest('execute', 'job-1', facts.signature),
        'refund-unregistered',
      ),
    ).rejects.toMatchObject({ code: 'refund_liability_not_found' });

    await expect(
      policy.registerRefundLiability(
        {
          ...signedRefundRequest('register', 'job-1', facts.signature),
          authorizationSignature: signedRefundRequest('execute', 'job-1', facts.signature)
            .authorizationSignature,
        },
        'register-wrong-action',
      ),
    ).rejects.toMatchObject({ code: 'refund_authorization_invalid' });

    await policy.registerRefundLiability(
      signedRefundRequest('register', 'job-1', facts.signature),
      'register-valid',
    );
    const tampered = {
      ...signedRefundRequest('execute', 'job-1', facts.signature),
      jobId: 'job-2',
    };
    await expect(policy.refund(tampered, 'refund-tampered')).rejects.toMatchObject({
      code: 'refund_authorization_invalid',
    });
  });

  it('rejects expired authorization and settlement outside the admitted payment window', async () => {
    const { chain, policy } = fixture();
    const facts = settlement();
    chain.settlements.set(facts.signature, facts);
    await expect(
      policy.registerRefundLiability(
        signedRefundRequest(
          'register',
          'job-expired-auth',
          facts.signature,
          new Date(Date.now() - 1).toISOString(),
        ),
        'register-expired-auth',
      ),
    ).rejects.toMatchObject({ code: 'refund_authorization_expired' });

    facts.blockTimeUnixSeconds -= 31;
    await expect(
      policy.registerRefundLiability(
        signedRefundRequest('register', 'job-historical', facts.signature),
        'register-historical',
      ),
    ).rejects.toMatchObject({ code: 'settlement_outside_payment_window' });
  });

  it('registers an admission-bound settlement after a 48-hour outage', async () => {
    let now = Date.now();
    const { chain, policy } = fixture({ now: () => new Date(now) });
    const facts = settlement();
    facts.blockTimeUnixSeconds = Math.floor(now / 1_000);
    chain.settlements.set(facts.signature, facts);
    now += 48 * 60 * 60 * 1_000;

    await expect(
      policy.registerRefundLiability(
        signedRefundRequest(
          'register',
          'job-recovered',
          facts.signature,
          new Date(now + 10 * 60_000).toISOString(),
        ),
        'register-recovered',
      ),
    ).resolves.toMatchObject({
      jobId: 'job-recovered',
      settlementSignature: facts.signature,
    });
  });

  it('binds one settlement to one job and reserves readiness capacity immediately', async () => {
    const { chain, policy } = fixture();
    const first = settlement();
    const second = settlement('7'.repeat(64));
    chain.settlements.set(first.signature, first);
    chain.settlements.set(second.signature, second);

    await policy.registerRefundLiability(
      signedRefundRequest('register', 'job-1', first.signature),
      'register-job-1',
    );
    await expect(
      policy.registerRefundLiability(
        signedRefundRequest('register', 'job-2', first.signature),
        'register-job-2',
      ),
    ).rejects.toMatchObject({ code: 'settlement_liability_conflict' });
    await expect(
      policy.registerRefundLiability(
        signedRefundRequest('register', 'job-1', second.signature),
        'register-job-1-again',
      ),
    ).rejects.toMatchObject({ code: 'job_liability_conflict' });

    await expect(policy.readiness()).resolves.toMatchObject({
      pendingRefundRaw: '2000000',
      remainingRefundLimitUsdCents: 9_800,
      availableRefundRaw: '98000000',
    });
  });

  it('atomically rejects liabilities that exceed protected treasury capacity', async () => {
    const { chain, policy } = fixture();
    chain.refundRawAmount = 3_000_000n;
    const first = settlement('6'.repeat(64), '2000000');
    const second = settlement('7'.repeat(64), '2000000');
    chain.settlements.set(first.signature, first);
    chain.settlements.set(second.signature, second);
    await policy.registerRefundLiability(
      signedRefundRequest('register', 'job-1', first.signature),
      'register-capacity-1',
    );
    await expect(
      policy.registerRefundLiability(
        signedRefundRequest('register', 'job-2', second.signature),
        'register-capacity-2',
      ),
    ).rejects.toMatchObject({ code: 'refund_pool_insufficient' });
  });

  it('discharges successful work only after independent merged-PR verification', async () => {
    const { chain, merges, policy } = fixture();
    const facts = settlement();
    chain.settlements.set(facts.signature, facts);
    const liability = await policy.registerRefundLiability(
      signedRefundRequest('register', 'job-success', facts.signature),
      'register-success',
    );
    await bindDelivery(policy, liability.id, 'job-success', facts.signature);
    const discharged = await policy.dischargeRefundLiability(
      liability.id,
      signedDischargeRequest('job-success', facts.signature, 'owner/repository', 23),
      'discharge-success',
    );

    expect(merges.repositoryRequests).toEqual([
      {
        repository: 'owner/repository',
        issueNumber: 17,
        pullRequestNumber: 23,
        deliveredCommitSha: 'a'.repeat(40),
        reviewedHeadSha: 'a'.repeat(40),
        reviewedBaseSha: 'd'.repeat(40),
        reviewedBaseRef: 'main',
        reviewedDiffHash: 'f'.repeat(64),
        notBefore: expect.any(Date),
      },
    ]);
    expect(discharged).toMatchObject({
      dischargedAt: expect.any(Date),
      dischargeEvidenceHash: expect.stringMatching(/^[a-f0-9]{64}$/),
    });
    await expect(policy.readiness()).resolves.toMatchObject({
      pendingRefundRaw: '0',
      remainingRefundLimitUsdCents: 9_800,
      availableRefundRaw: '98000000',
    });
    await expect(
      policy.refund(
        signedRefundRequest('execute', 'job-success', facts.signature),
        'refund-discharged',
      ),
    ).rejects.toMatchObject({ code: 'refund_liability_discharged' });
  });

  it('binds the reviewed commit before publication and makes the binding immutable', async () => {
    const { chain, merges, policy } = fixture();
    const facts = settlement();
    chain.settlements.set(facts.signature, facts);
    const liability = await policy.registerRefundLiability(
      signedRefundRequest('register', 'job-binding', facts.signature),
      'register-binding',
    );
    const request = signedDeliveryBindingRequest('job-binding', facts.signature);
    const bound = await policy.bindRefundLiabilityDelivery(
      liability.id,
      request,
      'stable-delivery-binding',
    );

    expect(merges.unpublishedRequests).toEqual([
      { repository: 'owner/repository', commitSha: 'a'.repeat(40) },
    ]);
    expect(bound).toMatchObject({
      reviewedHeadSha: 'a'.repeat(40),
      reviewedBaseSha: 'd'.repeat(40),
      reviewedBaseRef: 'main',
      reviewedDiffHash: 'f'.repeat(64),
      deliveryBoundAt: expect.any(Date),
      deliveryBindingHash: expect.stringMatching(/^[a-f0-9]{64}$/),
    });

    merges.error = new PolicyError('github_unavailable', 'unavailable', 503, true);
    await expect(
      policy.bindRefundLiabilityDelivery(liability.id, request, 'stable-delivery-binding'),
    ).resolves.toMatchObject({ id: liability.id });
    expect(merges.unpublishedRequests).toHaveLength(1);
    await expect(
      policy.bindRefundLiabilityDelivery(
        liability.id,
        signedDeliveryBindingRequest('job-binding', facts.signature, undefined, {
          reviewedHeadSha: 'b'.repeat(40),
        }),
        'different-delivery-binding',
      ),
    ).rejects.toMatchObject({ code: 'refund_liability_delivery_bound' });
  });

  it('rejects discharge without a binding and approve-A/merge-B substitution', async () => {
    const { chain, merges, policy } = fixture();
    const facts = settlement();
    chain.settlements.set(facts.signature, facts);
    const liability = await policy.registerRefundLiability(
      signedRefundRequest('register', 'job-substitution', facts.signature),
      'register-substitution',
    );
    await expect(
      policy.dischargeRefundLiability(
        liability.id,
        signedDischargeRequest('job-substitution', facts.signature, 'owner/repository', 23),
        'discharge-unbound',
      ),
    ).rejects.toMatchObject({ code: 'refund_liability_delivery_mismatch' });
    expect(merges.repositoryRequests).toHaveLength(0);

    await bindDelivery(policy, liability.id, 'job-substitution', facts.signature);
    const substituted = signedDischargeRequest(
      'job-substitution',
      facts.signature,
      'owner/repository',
      24,
    );
    merges.verifyRepositoryMerge = async (request) => ({
      repository: request.repository,
      issueNumber: request.issueNumber,
      pullRequestNumber: request.pullRequestNumber,
      pullRequestUrl: `https://github.com/${request.repository}/pull/${request.pullRequestNumber}`,
      mergeCommitOid: 'c'.repeat(40),
      headCommitOid: 'b'.repeat(40),
      baseCommitOid: request.reviewedBaseSha,
      baseRefName: request.reviewedBaseRef,
      diffHash: request.reviewedDiffHash,
      createdAt: new Date().toISOString(),
      mergedAt: new Date(Date.now() + 1_000).toISOString(),
    });
    await expect(
      policy.dischargeRefundLiability(liability.id, substituted, 'discharge-substituted-head'),
    ).rejects.toMatchObject({ code: 'github_evidence_mismatch', retryable: true });
  });

  it('rejects binding after the reviewed commit is already public', async () => {
    const { chain, merges, policy, store } = fixture();
    const facts = settlement();
    chain.settlements.set(facts.signature, facts);
    const liability = await policy.registerRefundLiability(
      signedRefundRequest('register', 'job-already-public', facts.signature),
      'register-already-public',
    );
    merges.error = new PolicyError('github_delivery_already_published', 'already public', 422);

    await expect(
      bindDelivery(policy, liability.id, 'job-already-public', facts.signature),
    ).rejects.toMatchObject({ code: 'github_delivery_already_published' });
    await expect(store.getRefundLiability(facts.signature)).resolves.toMatchObject({
      id: liability.id,
      deliveryBoundAt: null,
      reviewedHeadSha: null,
    });
  });

  it('rejects recycled PR evidence that predates the registered payment', async () => {
    const { chain, merges, policy } = fixture();
    const facts = settlement();
    chain.settlements.set(facts.signature, facts);
    const liability = await policy.registerRefundLiability(
      signedRefundRequest('register', 'job-old-pr', facts.signature),
      'register-old-pr',
    );
    await bindDelivery(policy, liability.id, 'job-old-pr', facts.signature);
    merges.verifyRepositoryMerge = async (request) => ({
      repository: request.repository,
      issueNumber: request.issueNumber,
      pullRequestNumber: request.pullRequestNumber,
      pullRequestUrl: `https://github.com/${request.repository}/pull/${request.pullRequestNumber}`,
      mergeCommitOid: 'a'.repeat(40),
      headCommitOid: request.deliveredCommitSha,
      baseCommitOid: request.reviewedBaseSha,
      baseRefName: request.reviewedBaseRef,
      diffHash: request.reviewedDiffHash,
      createdAt: new Date((facts.blockTimeUnixSeconds - 1) * 1_000).toISOString(),
      mergedAt: new Date((facts.blockTimeUnixSeconds + 1) * 1_000).toISOString(),
    });

    await expect(
      policy.dischargeRefundLiability(
        liability.id,
        signedDischargeRequest('job-old-pr', facts.signature, 'owner/repository', 23),
        'discharge-old-pr',
      ),
    ).rejects.toMatchObject({ code: 'github_pr_too_old' });
  });

  it('serializes refund execution and successful-work discharge to one outcome', async () => {
    const { chain, policy } = fixture();
    const facts = settlement();
    chain.settlements.set(facts.signature, facts);
    const liability = await policy.registerRefundLiability(
      signedRefundRequest('register', 'job-race', facts.signature),
      'register-race',
    );
    await bindDelivery(policy, liability.id, 'job-race', facts.signature);
    const outcomes = await Promise.allSettled([
      policy.refund(signedRefundRequest('execute', 'job-race', facts.signature), 'refund-race'),
      policy.dischargeRefundLiability(
        liability.id,
        signedDischargeRequest('job-race', facts.signature, 'owner/repository', 23),
        'discharge-race',
      ),
    ]);

    expect(outcomes.filter((result) => result.status === 'fulfilled')).toHaveLength(1);
    expect(outcomes.filter((result) => result.status === 'rejected')).toHaveLength(1);
    expect(outcomes.find((result) => result.status === 'rejected')).toMatchObject({
      reason: { code: expect.stringMatching(/refund_already_started|refund_liability_discharged/) },
    });
  });

  it('replays a discharge with fresh authorization and a stable idempotency key', async () => {
    let now = new Date('2026-08-22T12:00:00.000Z');
    const { chain, merges, policy } = fixture({ now: () => new Date(now) });
    const facts = {
      ...settlement(),
      blockTimeUnixSeconds: Math.floor(now.getTime() / 1_000),
    };
    chain.settlements.set(facts.signature, facts);
    const liability = await policy.registerRefundLiability(
      signedRefundRequest(
        'register',
        'job-discharge-retry',
        facts.signature,
        new Date(now.getTime() + 60_000).toISOString(),
      ),
      'register-discharge-retry',
    );
    await bindDelivery(
      policy,
      liability.id,
      'job-discharge-retry',
      facts.signature,
      new Date(now.getTime() + 60_000).toISOString(),
    );
    const first = await policy.dischargeRefundLiability(
      liability.id,
      signedDischargeRequest(
        'job-discharge-retry',
        facts.signature,
        'owner/repository',
        23,
        new Date(now.getTime() + 60_000).toISOString(),
      ),
      'stable-discharge-key',
    );

    now = new Date(now.getTime() + 120_000);
    merges.error = new PolicyError('github_unavailable', 'unavailable', 503, true);
    const replay = await policy.dischargeRefundLiability(
      liability.id,
      signedDischargeRequest(
        'job-discharge-retry',
        facts.signature,
        'owner/repository',
        23,
        new Date(now.getTime() + 60_000).toISOString(),
      ),
      'stable-discharge-key',
    );
    expect(replay.id).toBe(first.id);
    expect(replay.dischargeEvidenceHash).toBe(first.dischargeEvidenceHash);
  });

  it('derives the recipient and exact amount from independently read settlement facts', async () => {
    const { chain, policy } = fixture();
    const facts = settlement();
    chain.settlements.set(facts.signature, facts);

    const result = await registerAndRefund(policy, 'job-1', facts.signature, 'refund-key-1');

    expect(result.status).toBe('finalized');
    expect(result.recipient).toBe(PAYER);
    expect(result.amountUsdCents).toBe(200);
    expect(chain.preparedOperations).toContainEqual(
      expect.objectContaining({ payer: PAYER, rawAmount: '2000000' }),
    );
  });

  it.each([
    ['unfinalized', { finalized: false }, 'settlement_not_finalized'],
    ['failed', { succeeded: false }, 'settlement_failed'],
    ['wrong treasury', { recipient: '7'.repeat(32) }, 'wrong_treasury'],
    ['wrong mint', { mint: '8'.repeat(32) }, 'asset_not_allowed'],
    ['wrong decimals', { decimals: 9 }, 'asset_not_allowed'],
  ])('rejects %s settlement facts', async (_name, patch, expectedCode) => {
    const { chain, policy } = fixture();
    const facts = { ...settlement(), ...patch } as SettlementFacts;
    chain.settlements.set(facts.signature, facts);

    await expect(
      registerAndRefund(policy, 'job-1', facts.signature, 'refund-key-1'),
    ).rejects.toMatchObject({ code: expectedCode });
  });

  it('rejects a refund above the independent per-operation limit', async () => {
    const { chain, policy } = fixture();
    const facts = settlement('7'.repeat(64), '25000001');
    chain.settlements.set(facts.signature, facts);

    await expect(
      registerAndRefund(policy, 'job-1', facts.signature, 'refund-key-1'),
    ).rejects.toMatchObject({ code: 'operation_limit_exceeded' });
  });

  it('authorizes a settlement once under concurrent retries', async () => {
    const { chain, policy, store } = fixture();
    const facts = settlement();
    chain.settlements.set(facts.signature, facts);

    const registration = signedRefundRequest('register', 'job-1', facts.signature);
    await Promise.all(
      Array.from({ length: 30 }, () =>
        policy.registerRefundLiability(registration, 'liability-refund-key-1'),
      ),
    );
    const execution = signedRefundRequest('execute', 'job-1', facts.signature);
    const calls = Array.from({ length: 30 }, () => policy.refund(execution, 'refund-key-1'));
    const results = await Promise.all(calls);
    const final = await store.get(results[0].id);

    expect(new Set(results.map((result) => result.id))).toHaveLength(1);
    expect(final?.status).toBe('finalized');
    expect([...chain.applications.values()]).toEqual([1]);
  });

  it('rejects reuse of an idempotency key for a different settlement', async () => {
    const { chain, policy } = fixture();
    const first = settlement('8'.repeat(64));
    const second = settlement('9'.repeat(64));
    chain.settlements.set(first.signature, first);
    chain.settlements.set(second.signature, second);
    await policy.registerRefundLiability(
      signedRefundRequest('register', 'job-1', first.signature),
      'register-first',
    );
    await policy.registerRefundLiability(
      signedRefundRequest('register', 'job-2', second.signature),
      'register-second',
    );
    await policy.refund(signedRefundRequest('execute', 'job-1', first.signature), 'same-key');

    await expect(
      policy.refund(signedRefundRequest('execute', 'job-2', second.signature), 'same-key'),
    ).rejects.toMatchObject({ code: 'idempotency_conflict' });
  });

  it('does not alias a protected resource to a second idempotency key', async () => {
    const { chain, policy } = fixture();
    const facts = settlement();
    chain.settlements.set(facts.signature, facts);
    await policy.registerRefundLiability(
      signedRefundRequest('register', 'job-1', facts.signature),
      'register-alias',
    );
    await policy.refund(
      signedRefundRequest('execute', 'job-1', facts.signature),
      'refund-original-key',
    );

    await expect(
      policy.refund(signedRefundRequest('execute', 'job-1', facts.signature), 'refund-alias-key'),
    ).rejects.toMatchObject({ code: 'resource_conflict' });
  });

  it('enforces the rolling 24-hour limit atomically', async () => {
    const { chain, policy } = fixture();
    const signatures = ['A', 'B', 'C', 'D', 'E'].map((prefix) => prefix.repeat(64));
    for (const signature of signatures)
      chain.settlements.set(signature, settlement(signature, '25000000'));

    await Promise.all(
      signatures
        .slice(0, 4)
        .map((signature, index) =>
          registerAndRefund(policy, `job-${index}`, signature, `daily-key-${index}`),
        ),
    );
    await expect(
      registerAndRefund(policy, 'job-5', signatures[4], 'daily-key-5'),
    ).rejects.toMatchObject({ code: 'daily_limit_exceeded' });
  });

  it('reconciles a crash after broadcast without applying a second refund', async () => {
    const { chain, policy } = fixture();
    const facts = settlement();
    chain.settlements.set(facts.signature, facts);
    chain.throwAfterBroadcastOnce = true;

    const first = await registerAndRefund(policy, 'job-1', facts.signature, 'refund-key-1');
    expect(first.status).toBe('reconciling');

    const recovered = await policy.drive(first.id);
    expect(recovered.status).toBe('finalized');
    expect(chain.applications.get(recovered.transactionSignature!)).toBe(1);
  });

  it('persists signed bytes before broadcasting', async () => {
    const { chain, policy, store } = fixture();
    const facts = settlement();
    chain.settlements.set(facts.signature, facts);
    chain.autoFinalize = false;

    const result = await registerAndRefund(policy, 'job-1', facts.signature, 'refund-key-1');
    const stored = await store.get(result.id);

    expect(stored?.status).toBe('submitted');
    expect(stored?.prepared?.wireTransaction).toBeTruthy();
    expect(stored?.prepared?.signature).toBe(stored?.transactionSignature);
  });

  it('serves an exact idempotent retry without trusting RPC availability again', async () => {
    const { chain, policy } = fixture();
    const facts = settlement();
    chain.settlements.set(facts.signature, facts);
    await policy.registerRefundLiability(
      signedRefundRequest('register', 'job-1', facts.signature),
      'register-stable',
    );
    const execution = signedRefundRequest('execute', 'job-1', facts.signature);
    const first = await policy.refund(execution, 'refund-stable-key');
    chain.settlements.clear();

    const replay = await policy.refund(execution, 'refund-stable-key');
    expect(replay.id).toBe(first.id);
    expect(replay.status).toBe('finalized');
  });

  it('accepts fresh authorization on a stable idempotency key after the prior TTL', async () => {
    let now = new Date('2026-08-22T12:00:00.000Z');
    const { chain, policy } = fixture({ now: () => new Date(now) });
    const facts = {
      ...settlement(),
      blockTimeUnixSeconds: Math.floor(now.getTime() / 1_000),
    };
    chain.settlements.set(facts.signature, facts);
    const firstRegistration = signedRefundRequest(
      'register',
      'job-fresh-auth',
      facts.signature,
      new Date(now.getTime() + 60_000).toISOString(),
    );
    const registered = await policy.registerRefundLiability(
      firstRegistration,
      'stable-liability-key',
    );
    const firstExecution = signedRefundRequest(
      'execute',
      'job-fresh-auth',
      facts.signature,
      new Date(now.getTime() + 60_000).toISOString(),
    );
    const refunded = await policy.refund(firstExecution, 'stable-refund-key');

    now = new Date(now.getTime() + 120_000);
    chain.settlements.clear();
    const freshRegistration = signedRefundRequest(
      'register',
      'job-fresh-auth',
      facts.signature,
      new Date(now.getTime() + 60_000).toISOString(),
    );
    const freshExecution = signedRefundRequest(
      'execute',
      'job-fresh-auth',
      facts.signature,
      new Date(now.getTime() + 60_000).toISOString(),
    );

    await expect(
      policy.registerRefundLiability(freshRegistration, 'stable-liability-key'),
    ).resolves.toMatchObject({ id: registered.id });
    await expect(policy.refund(freshExecution, 'stable-refund-key')).resolves.toMatchObject({
      id: refunded.id,
      status: 'finalized',
    });
  });
});

describe('contributor escrow policy', () => {
  it('funds a claimant-free vault before the bounty can open', async () => {
    const { chain, policy } = fixture();
    const reserve = await policy.createEscrow(escrowRequest('bounty-1'), 'escrow-key-1');

    expect(reserve.status).toBe('finalized');
    expect(reserve.recipient).toBe('escrow-vault');
    expect(operationView(reserve)).toMatchObject({
      amountAtomic: '100000000',
    });
    expect(reserve.details).toMatchObject({
      escrowAddress: expect.any(String),
      vaultAddress: expect.any(String),
    });
    expect(chain.preparedOperations[0]).toEqual(
      expect.objectContaining({ kind: 'escrow_reserve', amountLamports: '100000000' }),
    );
  });

  it('separates protected refund capacity from the escrow spending cap', async () => {
    const { chain, policy } = fixture({ refundDailyLimit: 200, escrowDailyLimit: 1_000 });
    const facts = settlement('F'.repeat(64), '2000000');
    chain.settlements.set(facts.signature, facts);
    await registerAndRefund(policy, 'job-cap', facts.signature, 'refund-cap');

    await expect(
      policy.createEscrow(escrowRequest('bounty-cap'), 'escrow-cap'),
    ).resolves.toMatchObject({ status: 'finalized' });
  });

  it('rejects reserve value above either the operation or absolute asset ceiling', async () => {
    const overOperation = fixture();
    await expect(
      overOperation.policy.createEscrow(
        { ...escrowRequest('bounty-operation'), amountUsdCents: 2_501 },
        'escrow-operation',
      ),
    ).rejects.toMatchObject({ code: 'operation_limit_exceeded' });

    const overAsset = fixture({ maxEscrowLamports: 50_000_000 });
    await expect(
      overAsset.policy.createEscrow(escrowRequest('bounty-asset'), 'escrow-asset'),
    ).rejects.toMatchObject({ code: 'escrow_asset_limit_exceeded' });
  });

  it('rejects a reserve when SOL cannot cover principal, both rents, and fee reserve', async () => {
    const { chain, policy } = fixture();
    chain.escrowLamports = 102_999_999n;
    await expect(
      policy.createEscrow(escrowRequest('bounty-capacity'), 'escrow-capacity'),
    ).rejects.toMatchObject({ code: 'escrow_pool_insufficient' });
  });

  it('requires a signer-issued challenge and a valid claimant wallet signature', async () => {
    const { policy } = fixture();
    const reserve = await policy.createEscrow(escrowRequest('bounty-bind'), 'reserve-bind');
    const grant = await policy.issueGitHubIdentityGrant({ accessToken: 'o'.repeat(20) });
    const challenge = await policy.issueBindChallenge(reserve.id, {
      claimantWallet: CLAIMANT,
      githubGrantId: grant.id,
    });
    expect(challenge.message).toContain(`Reservation: ${reserve.id}`);
    expect(Date.parse(challenge.claimExpiresAt) - Date.now()).toBeGreaterThan(47 * 60 * 60 * 1000);

    await expect(
      policy.bindEscrow(
        reserve.id,
        { challengeId: challenge.id, signature: Buffer.alloc(64).toString('base64') },
        'bind-invalid',
      ),
    ).rejects.toMatchObject({ code: 'wallet_signature_invalid' });

    const bound = await policy.bindEscrow(
      reserve.id,
      { challengeId: challenge.id, signature: signChallenge(challenge.message) },
      'bind-valid',
    );
    expect(bound).toMatchObject({ kind: 'escrow_bind', status: 'finalized', recipient: CLAIMANT });
  });

  it('atomically consumes a challenge and permits only one claimant binding', async () => {
    const { policy } = fixture();
    const reserve = await policy.createEscrow(escrowRequest('bounty-bind-race'), 'reserve-race');
    const grant = await policy.issueGitHubIdentityGrant({ accessToken: 'o'.repeat(20) });
    const challenge = await policy.issueBindChallenge(reserve.id, {
      claimantWallet: CLAIMANT,
      githubGrantId: grant.id,
    });
    const request = { challengeId: challenge.id, signature: signChallenge(challenge.message) };
    const outcomes = await Promise.allSettled([
      policy.bindEscrow(reserve.id, request, 'bind-race-a'),
      policy.bindEscrow(reserve.id, request, 'bind-race-b'),
    ]);

    expect(outcomes.filter((result) => result.status === 'fulfilled')).toHaveLength(1);
    expect(outcomes.filter((result) => result.status === 'rejected')).toHaveLength(1);
  });

  it('independently verifies a short-lived GitHub OAuth identity grant', async () => {
    const { chain, merges, policy } = fixture();
    const reserve = await policy.createEscrow(escrowRequest('bounty-login'), 'reserve-login');
    const preparedBefore = chain.preparedOperations.length;
    merges.error = new PolicyError('github_identity_invalid', 'missing', 422);

    await expect(
      policy.issueGitHubIdentityGrant({ accessToken: 'o'.repeat(20) }),
    ).rejects.toMatchObject({ code: 'github_identity_invalid' });
    expect(chain.preparedOperations).toHaveLength(preparedBefore);
  });

  it('consumes a GitHub identity grant exactly once', async () => {
    const { policy } = fixture();
    const first = await policy.createEscrow(escrowRequest('bounty-grant-a'), 'reserve-grant-a');
    const second = await policy.createEscrow(escrowRequest('bounty-grant-b'), 'reserve-grant-b');
    const grant = await policy.issueGitHubIdentityGrant({ accessToken: 'o'.repeat(20) });
    await policy.issueBindChallenge(first.id, {
      claimantWallet: CLAIMANT,
      githubGrantId: grant.id,
    });
    await expect(
      policy.issueBindChallenge(second.id, {
        claimantWallet: CLAIMANT,
        githubGrantId: grant.id,
      }),
    ).rejects.toMatchObject({ code: 'github_grant_consumed' });
  });

  it('persists independent merge evidence before release broadcast', async () => {
    const { policy, store, merges } = fixture();
    const { reserve } = await reserveAndBind(policy, 'bounty-evidence');
    const release = await policy.releaseEscrow(reserve.id, releaseRequest(), 'release-evidence');
    const stored = await store.get(release.id);
    const resolution = stored?.details.resolution as Record<string, unknown>;

    expect(merges.requests).toHaveLength(1);
    expect(resolution.mergeReceiptHash).toMatch(/^[a-f0-9]{64}$/);
    expect(resolution.evidence).toMatchObject({
      repository: 'owner/repository',
      issueNumber: 17,
      claimantGitHubLogin: 'contributor',
      pullRequestNumber: 23,
      headCommitOid: 'b'.repeat(40),
      baseCommitOid: 'd'.repeat(40),
      baseRefName: 'main',
      diffHash: 'c'.repeat(64),
    });
    expect(stored?.prepared?.wireTransaction).toBeTruthy();
  });

  it('replays only the exact reviewed revision for a release key', async () => {
    const { policy, merges } = fixture();
    const { reserve } = await reserveAndBind(policy, 'bounty-release-replay');
    const input = releaseRequest();
    const released = await policy.releaseEscrow(reserve.id, input, 'release-review-bound');

    await expect(
      policy.releaseEscrow(reserve.id, input, 'release-review-bound'),
    ).resolves.toMatchObject({ id: released.id, status: 'finalized' });
    expect(merges.requests).toHaveLength(1);
    await expect(
      policy.releaseEscrow(
        reserve.id,
        { ...input, reviewedHeadSha: 'd'.repeat(40) },
        'release-review-bound',
      ),
    ).rejects.toMatchObject({ code: 'idempotency_conflict' });
  });

  it('does not release without a finalized claimant binding', async () => {
    const { policy } = fixture();
    const reserve = await policy.createEscrow(escrowRequest('bounty-unbound'), 'reserve-unbound');
    await expect(
      policy.releaseEscrow(reserve.id, releaseRequest(), 'release-unbound'),
    ).rejects.toMatchObject({ code: 'escrow_not_bound' });
  });

  it('uses offer expiry for unbound refunds at the exact boundary', async () => {
    let now = new Date('2026-08-22T12:00:00.000Z');
    const { chain, policy } = fixture({ now: () => new Date(now) });
    const reserve = await policy.createEscrow(
      escrowRequest('bounty-unbound-expiry', '2026-08-22T14:00:00.000Z'),
      'reserve-unbound-expiry',
    );
    now = new Date('2026-08-22T13:59:59.999Z');
    await expect(
      policy.refundEscrow(reserve.id, { reasonCode: 'expired' }, 'refund-too-soon'),
    ).rejects.toMatchObject({ code: 'escrow_not_expired' });
    now = new Date('2026-08-22T14:00:00.000Z');
    await expect(
      policy.refundEscrow(reserve.id, { reasonCode: 'expired' }, 'refund-boundary'),
    ).resolves.toMatchObject({ status: 'finalized' });
    expect(chain.preparedOperations.at(-1)?.kind).toBe('escrow_refund');
  });

  it('uses the immutable 48-hour binding expiry instead of offer expiry', async () => {
    let now = new Date('2026-08-22T12:00:00.000Z');
    const { policy } = fixture({ now: () => new Date(now) });
    const { reserve, challenge } = await reserveAndBind(
      policy,
      'bounty-bound-expiry',
      '2026-08-22T14:00:00.000Z',
    );
    now = new Date('2026-08-22T14:00:00.000Z');
    await expect(
      policy.refundEscrow(reserve.id, { reasonCode: 'expired' }, 'bound-offer-expiry'),
    ).rejects.toMatchObject({ code: 'escrow_not_expired' });
    now = new Date(challenge.claimExpiresAt);
    await expect(
      policy.refundEscrow(reserve.id, { reasonCode: 'expired' }, 'bound-claim-expiry'),
    ).resolves.toMatchObject({ status: 'finalized' });
  });

  it('serializes concurrent release and eligible refund to one terminal resolution', async () => {
    let now = new Date('2026-08-22T12:00:00.000Z');
    const { policy } = fixture({ now: () => new Date(now) });
    const { reserve, challenge } = await reserveAndBind(policy, 'bounty-resolution-race');
    now = new Date(challenge.claimExpiresAt);
    const outcomes = await Promise.allSettled([
      policy.releaseEscrow(reserve.id, releaseRequest(), 'release-race'),
      policy.refundEscrow(reserve.id, { reasonCode: 'expired' }, 'refund-race'),
    ]);

    expect(outcomes.filter((result) => result.status === 'fulfilled')).toHaveLength(1);
    expect(outcomes.filter((result) => result.status === 'rejected')).toHaveLength(1);
    expect(outcomes.find((result) => result.status === 'fulfilled')).toMatchObject({
      value: { kind: 'escrow_refund' },
    });
    expect(outcomes.find((result) => result.status === 'rejected')).toMatchObject({
      reason: { code: 'escrow_claim_expired' },
    });
  });

  it('rejects a PR that merged after the immutable claim expiry', async () => {
    const { merges, policy } = fixture();
    const { reserve, challenge } = await reserveAndBind(policy, 'bounty-late-merge');
    const original = merges.verify.bind(merges);
    merges.verify = async (request) => ({
      ...(await original(request)),
      mergedAt: new Date(Date.parse(challenge.claimExpiresAt) + 1).toISOString(),
    });

    await expect(
      policy.releaseEscrow(reserve.id, releaseRequest(), 'release-late-merge'),
    ).rejects.toMatchObject({ code: 'github_merge_after_expiry' });
  });

  it('rejects an expired missing release transaction and hands resolution to refund', async () => {
    let now = new Date('2026-08-22T12:00:00.000Z');
    const { chain, policy } = fixture({ now: () => new Date(now) });
    const { reserve, challenge } = await reserveAndBind(policy, 'bounty-release-handoff');
    chain.autoFinalize = false;
    const release = await policy.releaseEscrow(reserve.id, releaseRequest(), 'release-handoff');
    expect(release.status).toBe('submitted');
    chain.states.delete(release.transactionSignature!);
    chain.currentBlockHeight = release.prepared!.lastValidBlockHeight + 1;
    now = new Date(challenge.claimExpiresAt);

    const expired = await policy.drive(release.id);
    expect(expired).toMatchObject({
      status: 'rejected',
      errorCode: 'release_deadline_elapsed',
    });
    chain.autoFinalize = true;
    await expect(
      policy.refundEscrow(reserve.id, { reasonCode: 'expired' }, 'refund-after-handoff'),
    ).resolves.toMatchObject({ kind: 'escrow_refund', status: 'finalized' });
  });

  it('does not let a permanent prepare failure occupy the resolution resource forever', async () => {
    let now = new Date('2026-08-22T12:00:00.000Z');
    const { chain, policy } = fixture({ now: () => new Date(now) });
    const { reserve, challenge } = await reserveAndBind(policy, 'bounty-prepare-rejection');
    const originalPrepare = chain.prepare.bind(chain);
    chain.prepare = async (operation) => {
      if (operation.kind === 'escrow_release') {
        throw new PolicyError('transaction_form_not_allowed', 'invalid release form', 403);
      }
      return originalPrepare(operation);
    };
    const release = await policy.releaseEscrow(
      reserve.id,
      releaseRequest(),
      'release-invalid-form',
    );
    expect(release).toMatchObject({
      status: 'rejected',
      errorCode: 'transaction_form_not_allowed',
    });

    chain.prepare = originalPrepare;
    now = new Date(challenge.claimExpiresAt);
    await expect(
      policy.refundEscrow(reserve.id, { reasonCode: 'expired' }, 'refund-after-rejection'),
    ).resolves.toMatchObject({ kind: 'escrow_refund', status: 'finalized' });
  });

  it('never permits the same external bounty ID to fund a second vault', async () => {
    const { policy } = fixture();
    await policy.createEscrow(escrowRequest('bounty-tombstone'), 'reserve-first');
    await expect(
      policy.createEscrow(escrowRequest('bounty-tombstone'), 'reserve-second'),
    ).rejects.toMatchObject({ code: 'resource_conflict' });
  });
});

describe('store policy invariants', () => {
  it('does not release a daily reservation when an operation is finalized', async () => {
    const { chain, policy } = fixture({ dailyLimit: 200 });
    const first = settlement('C'.repeat(64), '2000000');
    const second = settlement('D'.repeat(64), '10000');
    chain.settlements.set(first.signature, first);
    chain.settlements.set(second.signature, second);
    await registerAndRefund(policy, 'job-1', first.signature, 'refund-key-1');

    await expect(
      registerAndRefund(policy, 'job-2', second.signature, 'refund-key-2'),
    ).rejects.toBeInstanceOf(PolicyError);
  });
});

function escrowRequest(
  bountyId: string,
  expiresAt = new Date(Date.now() + 2 * 60 * 60 * 1000).toISOString(),
) {
  return {
    bountyId,
    amountUsdCents: 1_000,
    acceptanceHash: 'a'.repeat(64),
    expiresAt,
    repository: 'owner/repository',
    issueNumber: 17,
  };
}

async function reserveAndBind(policy: PolicyService, bountyId: string, offerExpiresAt?: string) {
  const reserve = await policy.createEscrow(
    escrowRequest(bountyId, offerExpiresAt),
    `reserve-${bountyId}`,
  );
  const grant = await policy.issueGitHubIdentityGrant({ accessToken: 'o'.repeat(20) });
  const challenge = await policy.issueBindChallenge(reserve.id, {
    claimantWallet: CLAIMANT,
    githubGrantId: grant.id,
  });
  const binding = await policy.bindEscrow(
    reserve.id,
    { challengeId: challenge.id, signature: signChallenge(challenge.message) },
    `bind-${bountyId}`,
  );
  return { reserve, challenge, binding };
}

function signChallenge(message: string): string {
  return signWithKey(CLAIMANT_KEYPAIR, message);
}

function signedRefundRequest(
  action: 'register',
  jobId: string,
  settlementSignature: string,
  authorizationExpiresAt?: string,
): RegisterRefundLiabilityRequest;
function signedRefundRequest(
  action: 'execute',
  jobId: string,
  settlementSignature: string,
  authorizationExpiresAt?: string,
): RefundRequest;
function signedRefundRequest(
  action: 'register' | 'execute',
  jobId: string,
  settlementSignature: string,
  authorizationExpiresAt = new Date(Date.now() + 10 * 60 * 1_000).toISOString(),
): RefundRequest | RegisterRefundLiabilityRequest {
  const unsigned = {
    jobId,
    settlementSignature,
    ...(action === 'register'
      ? {
          repositoryAdmissionId: DEFAULT_ADMISSION_ID,
          repositoryAdmissionEvidenceHash: defaultAdmissionEvidenceHash,
          repository: 'owner/repository',
          issueNumber: 17,
          baseRef: 'main',
          baseSha: 'd'.repeat(40),
          repositoryAuthorizedAt: '2026-08-22T11:00:00.000Z',
          authorizationEvidenceHash: 'e'.repeat(64),
        }
      : {}),
    authorizationExpiresAt,
  };
  return {
    ...unsigned,
    authorizationSignature: signWithKey(
      JOB_AUTHORITY,
      refundAuthorizationMessage(action, unsigned),
    ),
  };
}

function signedDischargeRequest(
  jobId: string,
  settlementSignature: string,
  repository: string,
  pullRequestNumber: number,
  authorizationExpiresAt = new Date(Date.now() + 10 * 60 * 1_000).toISOString(),
  overrides: Partial<
    Pick<
      DischargeRefundLiabilityRequest,
      | 'issueNumber'
      | 'deliveredCommitSha'
      | 'reviewedHeadSha'
      | 'reviewedBaseSha'
      | 'reviewedBaseRef'
      | 'reviewedDiffHash'
    >
  > = {},
): DischargeRefundLiabilityRequest {
  const unsigned = {
    jobId,
    settlementSignature,
    repository,
    issueNumber: 17,
    pullRequestNumber,
    deliveredCommitSha: 'a'.repeat(40),
    reviewedHeadSha: 'a'.repeat(40),
    reviewedBaseSha: 'd'.repeat(40),
    reviewedBaseRef: 'main',
    reviewedDiffHash: 'f'.repeat(64),
    ...overrides,
    authorizationExpiresAt,
  };
  return {
    ...unsigned,
    authorizationSignature: signWithKey(
      JOB_AUTHORITY,
      refundDischargeAuthorizationMessage(unsigned),
    ),
  };
}

function signedDeliveryBindingRequest(
  jobId: string,
  settlementSignature: string,
  authorizationExpiresAt = new Date(Date.now() + 10 * 60 * 1_000).toISOString(),
  overrides: Partial<
    Pick<
      BindRefundLiabilityDeliveryRequest,
      'reviewedHeadSha' | 'reviewedBaseSha' | 'reviewedBaseRef' | 'reviewedDiffHash'
    >
  > = {},
): BindRefundLiabilityDeliveryRequest {
  const unsigned = {
    jobId,
    settlementSignature,
    reviewedHeadSha: 'a'.repeat(40),
    reviewedBaseSha: 'd'.repeat(40),
    reviewedBaseRef: 'main',
    reviewedDiffHash: 'f'.repeat(64),
    ...overrides,
    authorizationExpiresAt,
  };
  return {
    ...unsigned,
    authorizationSignature: signWithKey(
      JOB_AUTHORITY,
      refundDeliveryBindingAuthorizationMessage(unsigned),
    ),
  };
}

async function bindDelivery(
  policy: PolicyService,
  liabilityId: string,
  jobId: string,
  settlementSignature: string,
  authorizationExpiresAt?: string,
) {
  return policy.bindRefundLiabilityDelivery(
    liabilityId,
    signedDeliveryBindingRequest(jobId, settlementSignature, authorizationExpiresAt),
    `delivery-${jobId}`,
  );
}

async function registerAndRefund(
  policy: PolicyService,
  jobId: string,
  settlementSignature: string,
  idempotencyKey: string,
) {
  await policy.registerRefundLiability(
    signedRefundRequest('register', jobId, settlementSignature),
    `liability-${idempotencyKey}`,
  );
  return policy.refund(signedRefundRequest('execute', jobId, settlementSignature), idempotencyKey);
}

function signWithKey(keypair: Keypair, message: string): string {
  const seed = keypair.secretKey.subarray(0, 32);
  const pkcs8 = Buffer.concat([
    Buffer.from('302e020100300506032b657004220420', 'hex'),
    Buffer.from(seed),
  ]);
  return sign(
    null,
    Buffer.from(message, 'utf8'),
    createPrivateKey({ key: pkcs8, format: 'der', type: 'pkcs8' }),
  ).toString('base64');
}
