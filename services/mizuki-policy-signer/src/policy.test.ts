import { createHash, createPrivateKey, randomUUID, sign } from 'node:crypto';
import {
  Keypair,
  PublicKey,
  SystemProgram,
  TransactionInstruction,
  TransactionMessage,
  VersionedTransaction,
} from '@solana/web3.js';
import { describe, expect, it } from 'vitest';
import {
  authorizedSettlementTransaction,
  FixedUsdPriceOracle,
  MockChainGateway,
  type UsdPriceOracle,
} from './chain.js';
import type {
  BindRefundLiabilityDeliveryRequest,
  CreatePaymentIntentRequest,
  DischargeRefundLiabilityRequest,
  RefundRequest,
  RegisterRefundLiabilityRequest,
  RepositoryAdmission,
  RepositoryAdmissionRequest,
  SettlementFacts,
} from './domain.js';
import {
  operationView,
  paymentIntentAuthorizationMessage,
  PAYMENT_AUTHORIZATION_MAX_BYTES,
  PolicyError,
  escrowAcceptanceHash,
  escrowReleaseAuthorizationMessage,
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
import { MockIndependentReviewer } from './reviewer.js';
import { InMemoryOperationStore } from './store.js';

const TREASURY = '2'.repeat(32);
const MINT = '3'.repeat(32);
const PAYER = '4'.repeat(32);
const CLAIMANT_KEYPAIR = Keypair.generate();
const CLAIMANT = CLAIMANT_KEYPAIR.publicKey.toBase58();
const JOB_AUTHORITY = Keypair.generate();
const DEFAULT_ADMISSION_ID = '99999999-9999-4999-8999-999999999999';
let defaultAdmissionEvidenceHash = '';

function releaseRequest(
  escrowOperationId: string,
  reviewedAt: string,
  authorizationNow = new Date(),
  pullRequestNumber = 23,
) {
  const unsigned = {
    repository: 'owner/repository',
    issueNumber: 17,
    pullRequestNumber,
    mergeCommitSha: 'a'.repeat(40),
    reviewedHeadSha: 'b'.repeat(40),
    reviewedBaseSha: 'd'.repeat(40),
    reviewedBaseRef: 'main',
    reviewedDiffHash: 'c'.repeat(64),
    reviewReceiptId: '77777777-7777-4777-8777-777777777777',
    reviewReceiptHash: 'e'.repeat(64),
    reviewModel: 'independent-reviewer',
    reviewRoute: 'marketplace' as const,
    reviewedAt,
    authorizationExpiresAt: new Date(authorizationNow.getTime() + 10 * 60_000).toISOString(),
  };
  return {
    ...unsigned,
    authorizationSignature: signWithKey(
      JOB_AUTHORITY,
      escrowReleaseAuthorizationMessage(escrowOperationId, unsigned),
    ),
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
  const now = () => options.now?.() ?? new Date();
  const store = new InMemoryOperationStore();
  const chain = new MockChainGateway();
  chain.now = () => now().getTime();
  const metrics = new SignerMetrics();
  const merges = new MockMergeVerifier();
  const reviewer = new MockIndependentReviewer();
  const policy = new PolicyService(
    {
      refundTreasury: options.refundTreasury ?? TREASURY,
      escrowAuthority: options.escrowAuthority ?? '5'.repeat(32),
      refundMint: MINT,
      refundDecimals: 6,
      jobAuthorityPublicKey: options.jobAuthorityPublicKey ?? JOB_AUTHORITY.publicKey.toBase58(),
      reviewModel: 'independent-reviewer',
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
    reviewer,
    metrics,
    options.now,
  );
  const admittedAt = now();
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
    settlementPayer: settlementIdentity.payer,
    settlementMemo: settlementIdentity.memo,
    settlementRawAmount: '2000000',
    paymentWindowStartUnixSeconds: Math.floor(admittedAt.getTime() / 1_000) - 30,
    paymentWindowEndUnixSeconds: Math.floor(admittedAt.getTime() / 1_000) + 330,
  };
  defaultAdmissionEvidenceHash = requestHash({
    version: 2,
    ...identity,
    ...binding,
    verifierAppId: '12345',
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
      checks: 'read',
      contents: 'read',
      issues: 'read',
      metadata: 'read',
      pull_requests: 'read',
      statuses: 'read',
    },
    tokenRepositories: 1,
    tokenExpiresAt,
    admittedAt,
    evidenceHash: defaultAdmissionEvidenceHash,
  });
  return { store, chain, metrics, merges, reviewer, policy, now };
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
      new TransactionInstruction({
        programId: new PublicKey('MemoSq4gqABAXKb96qnH8TysNcWxMyWCqXgDLGmfcHr'),
        keys: [],
        data: Buffer.from(`mizuki:payment:v1:${quoteId}`),
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
      escrowRollingLimitUsdCents: 10_000,
      rollingEscrowSpendUsdCents: null,
      remainingEscrowLimitUsdCents: null,
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
  it('keeps an unresolved intent reserved after its payment window', async () => {
    let now = new Date('2026-08-22T12:00:00.000Z');
    const { chain, policy, store } = fixture({ now: () => new Date(now) });
    const intent = await policy.createPaymentIntent(
      signedPaymentIntentRequest(
        'job-window-authority',
        new Date(now.getTime() + 10 * 60_000).toISOString(),
      ),
      'payment-intent-window-authority',
    );

    now = new Date(intent.paymentWindowEndUnixSeconds * 1_000);
    await expect(
      policy.reconcilePaymentIntent(intent.id, 'reconcile-at-window-end'),
    ).rejects.toMatchObject({ code: 'payment_intent_pending' });

    now = new Date(intent.paymentWindowEndUnixSeconds * 1_000 + 1_000);
    await expect(
      policy.reconcilePaymentIntent(intent.id, 'reconcile-after-window-end'),
    ).rejects.toMatchObject({ code: 'payment_intent_pending' });

    now = new Date(intent.paymentWindowEndUnixSeconds * 1_000 + 24 * 60 * 60_000);
    await expect(
      policy.reconcilePaymentIntent(intent.id, 'reconcile-day-after-window-end'),
    ).rejects.toMatchObject({ code: 'payment_intent_pending' });
    await expect(store.getPaymentIntent(intent.id)).resolves.toMatchObject({
      status: 'reserved',
      expiredAt: null,
    });
    await expect(policy.readiness()).resolves.toMatchObject({
      pendingRefundRaw: intent.rawAmount,
      pendingRefundCount: 1,
    });
    expect(await store.pendingBountyReserveLamports()).toBe(intent.bountyReserveLamports);

    const admission = await store.getRepositoryAdmission(intent.repositoryAdmissionId);
    const facts = {
      ...settlement('Z'.repeat(64)),
      payer: admission!.settlementPayer!,
      blockTimeUnixSeconds: intent.paymentWindowStartUnixSeconds,
    };
    chain.reconciledSettlements.set(intent.signedMessageHash, facts);

    await expect(
      policy.reconcilePaymentIntent(intent.id, 'reconcile-late-finalization'),
    ).resolves.toMatchObject({
      paymentIntent: { status: 'activated', settlementSignature: facts.signature },
      refundLiability: { settlementSignature: facts.signature },
    });
  });

  it('does not poison a concurrent late finalization after a scan miss', async () => {
    let now = new Date('2026-08-22T12:00:00.000Z');
    const { chain, policy, store } = fixture({ now: () => new Date(now) });
    const intent = await policy.createPaymentIntent(
      signedPaymentIntentRequest(
        'job-window-race',
        new Date(now.getTime() + 10 * 60_000).toISOString(),
      ),
      'payment-intent-window-race',
    );
    const admission = await store.getRepositoryAdmission(intent.repositoryAdmissionId);
    const facts = {
      ...settlement('Y'.repeat(64)),
      payer: admission!.settlementPayer!,
      blockTimeUnixSeconds: intent.paymentWindowStartUnixSeconds,
    };
    let releaseMiss!: () => void;
    let reportMissStarted!: () => void;
    const missHeld = new Promise<void>((resolve) => {
      releaseMiss = resolve;
    });
    const missStarted = new Promise<void>((resolve) => {
      reportMissStarted = resolve;
    });
    let reconciliation = 0;
    chain.reconcileSettlement = async () => {
      reconciliation += 1;
      if (reconciliation === 1) {
        reportMissStarted();
        await missHeld;
        throw new PolicyError(
          'settlement_not_found',
          'A finalized settlement for this payment authorization was not found',
          422,
          true,
        );
      }
      return facts;
    };
    now = new Date(intent.paymentWindowEndUnixSeconds * 1_000 + 1_000);

    const missed = policy.reconcilePaymentIntent(intent.id, 'reconcile-window-race-miss').then(
      (value) => ({ value }),
      (error: unknown) => ({ error }),
    );
    await missStarted;
    await expect(
      policy.reconcilePaymentIntent(intent.id, 'reconcile-window-race-finalized'),
    ).resolves.toMatchObject({
      paymentIntent: { status: 'activated', settlementSignature: facts.signature },
    });
    releaseMiss();

    await expect(missed).resolves.toMatchObject({
      error: { code: 'payment_intent_pending' },
    });
    await expect(store.getPaymentIntent(intent.id)).resolves.toMatchObject({
      status: 'activated',
      settlementSignature: facts.signature,
    });
    await expect(store.pendingRefundCount()).resolves.toBe(1);
  });

  it('reserves refund and bounty capacity before settlement and activates one liability', async () => {
    const { chain, policy, store } = fixture();
    const admission = await store.getRepositoryAdmission(DEFAULT_ADMISSION_ID);
    expect(admission?.settlementPayer).toBeTruthy();

    const intent = await policy.createPaymentIntent(
      signedPaymentIntentRequest('job-protected'),
      'payment-intent-job-protected',
    );
    expect(intent).toMatchObject({
      status: 'reserved',
      rawAmount: '2000000',
      amountUsdCents: 200,
      bountyAmountUsdCents: 1_000,
      memo: `mizuki:payment:v1:${DEFAULT_ADMISSION_ID}`,
    });
    await expect(policy.readiness()).resolves.toMatchObject({
      pendingRefundRaw: '2000000',
      pendingRefundCount: 1,
    });

    const facts = {
      ...settlement(),
      payer: admission!.settlementPayer!,
    };
    chain.settlements.set(facts.signature, facts);
    const activated = await policy.activatePaymentIntent(
      intent.id,
      { settlementSignature: facts.signature },
      'activate-job-protected',
    );

    expect(activated.paymentIntent).toMatchObject({
      status: 'activated',
      settlementSignature: facts.signature,
    });
    expect(activated.refundLiability).toMatchObject({
      jobId: 'job-protected',
      settlementSignature: facts.signature,
      rawAmount: '2000000',
    });
    await expect(policy.readiness()).resolves.toMatchObject({
      pendingRefundRaw: '2000000',
      pendingRefundCount: 1,
    });
  });

  it('releases reserved bounty capacity after successful work discharges the liability', async () => {
    const { chain, policy, store } = fixture();
    const admission = await store.getRepositoryAdmission(DEFAULT_ADMISSION_ID);
    const intent = await policy.createPaymentIntent(
      signedPaymentIntentRequest('job-bounty-success'),
      'payment-intent-bounty-success',
    );
    expect(await store.pendingBountyReserveLamports()).toBe('100000000');

    const facts = {
      ...settlement('7'.repeat(64)),
      payer: admission!.settlementPayer!,
    };
    chain.settlements.set(facts.signature, facts);
    const activation = await policy.activatePaymentIntent(
      intent.id,
      { settlementSignature: facts.signature },
      'activate-bounty-success',
    );
    await bindDelivery(policy, activation.refundLiability.id, intent.jobId, facts.signature);
    await policy.dischargeRefundLiability(
      activation.refundLiability.id,
      signedDischargeRequest(intent.jobId, facts.signature, 'owner/repository', 23),
      'discharge-bounty-success',
    );

    expect(await store.pendingBountyReserveLamports()).toBe('0');
  });

  it('hands reserved bounty capacity to escrow only after a finalized refund', async () => {
    const { chain, policy, store } = fixture();
    const admission = await store.getRepositoryAdmission(DEFAULT_ADMISSION_ID);
    const intent = await policy.createPaymentIntent(
      signedPaymentIntentRequest('job-bounty-refund'),
      'payment-intent-bounty-refund',
    );
    const facts = {
      ...settlement('8'.repeat(64)),
      payer: admission!.settlementPayer!,
    };
    chain.settlements.set(facts.signature, facts);
    await policy.activatePaymentIntent(
      intent.id,
      { settlementSignature: facts.signature },
      'activate-bounty-refund',
    );

    const escrow = escrowRequest('bounty-refund', undefined, {
      sourceJobId: intent.jobId,
    });
    await expect(policy.createEscrow(escrow, 'escrow-before-refund')).rejects.toMatchObject({
      code: 'bounty_reserve_not_refunded',
    });
    await policy.refund(
      signedRefundRequest('execute', intent.jobId, facts.signature),
      'refund-bounty-source',
    );
    expect(await store.pendingBountyReserveLamports()).toBe('100000000');

    await expect(policy.createEscrow(escrow, 'escrow-after-refund')).resolves.toMatchObject({
      status: 'finalized',
    });
    expect(await store.pendingBountyReserveLamports()).toBe('0');
  });

  it('does not spend another job bounty reserve after adverse SOL price movement', async () => {
    let priceUsdMicros = 100_000_000;
    const context = fixture({
      prices: {
        solUsd: async () => ({ priceUsdMicros, observedAt: new Date() }),
      },
    });
    const request = escrowRequest('bounty-price-drift');
    await protectBountySource(context, request);
    expect(await context.store.pendingBountyReserveLamports()).toBe('100000000');

    priceUsdMicros = 50_000_000;
    await expect(context.policy.createEscrow(request, 'bounty-price-drift')).rejects.toMatchObject({
      code: 'bounty_reserve_price_drift',
      retryable: true,
    });
    expect(await context.store.pendingBountyReserveLamports()).toBe('100000000');
    expect(
      context.chain.preparedOperations.filter(({ kind }) => kind === 'escrow_reserve'),
    ).toHaveLength(0);
  });

  it('restores bounty capacity only after an escrow refund finalizes and replaces it once', async () => {
    let now = new Date('2026-08-22T12:00:00.000Z');
    const context = fixture({ now: () => new Date(now) });
    const { chain, policy, store } = context;
    const firstRequest = escrowRequest('bounty-lifecycle-first', '2026-08-22T14:00:00.000Z');
    await protectBountySource(context, firstRequest);
    expect(await store.pendingBountyReserveLamports()).toBe('100000000');

    const first = await policy.createEscrow(firstRequest, 'bounty-lifecycle-first');
    expect(await store.pendingBountyReserveLamports()).toBe('0');

    now = new Date(firstRequest.expiresAt);
    chain.autoFinalize = false;
    const refund = await policy.refundEscrow(
      first.id,
      { reasonCode: 'expired' },
      'bounty-lifecycle-refund',
    );
    expect(refund.status).toBe('submitted');
    expect(await store.pendingBountyReserveLamports()).toBe('0');
    await expect(policy.readiness()).resolves.toMatchObject({ healthy: false });

    chain.states.set(refund.transactionSignature!, 'finalized');
    chain.autoFinalize = true;
    await expect(policy.drive(refund.id)).resolves.toMatchObject({ status: 'finalized' });
    expect(await store.pendingBountyReserveLamports()).toBe('100000000');

    const replacement = await policy.createEscrow(
      escrowRequest('bounty-lifecycle-replacement', undefined, {
        sourceJobId: firstRequest.sourceJobId,
      }),
      'bounty-lifecycle-replacement',
    );
    expect(replacement.status).toBe('finalized');
    expect(await store.pendingBountyReserveLamports()).toBe('0');
  });

  it('atomically assigns one active escrow per refunded source job', async () => {
    const context = fixture();
    const { chain, policy } = context;
    const left = escrowRequest('bounty-handoff-left');
    await protectBountySource(context, left);
    const right = escrowRequest('bounty-handoff-right', undefined, {
      sourceJobId: left.sourceJobId,
    });

    const outcomes = await Promise.allSettled([
      policy.createEscrow(left, 'bounty-handoff-left'),
      policy.createEscrow(right, 'bounty-handoff-right'),
    ]);
    const winner = outcomes.find(
      (
        result,
      ): result is PromiseFulfilledResult<Awaited<ReturnType<PolicyService['createEscrow']>>> =>
        result.status === 'fulfilled',
    );
    expect(winner).toBeDefined();
    expect(outcomes.filter((result) => result.status === 'fulfilled')).toHaveLength(1);
    expect(outcomes.find((result) => result.status === 'rejected')).toMatchObject({
      reason: { code: 'bounty_handoff_active' },
    });
    expect(
      chain.preparedOperations.filter((operation) => operation.kind === 'escrow_reserve'),
    ).toHaveLength(1);
    const winningRequest = winner!.value.details.bountyId === left.bountyId ? left : right;
    await expect(
      policy.createEscrow(
        winningRequest,
        winner!.value.details.bountyId === left.bountyId
          ? 'bounty-handoff-left'
          : 'bounty-handoff-right',
      ),
    ).resolves.toMatchObject({ id: winner!.value.id });
  });

  it('rejects a payment intent when signer SOL cannot cover fees and recipient ATA rent', async () => {
    const { chain, policy } = fixture();
    chain.refundSignerLamports = chain.refundAtaRentLamports;

    await expect(
      policy.createPaymentIntent(
        signedPaymentIntentRequest('job-no-refund-sol'),
        'payment-intent-no-refund-sol',
      ),
    ).rejects.toMatchObject({ code: 'refund_signer_sol_insufficient' });
  });

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

  it('creates a second durable attempt after a safely failed refund transaction', async () => {
    const { chain, policy, store } = fixture();
    const facts = settlement();
    chain.settlements.set(facts.signature, facts);
    await policy.registerRefundLiability(
      signedRefundRequest('register', 'job-retry-refund', facts.signature),
      'register-retry-refund',
    );
    chain.autoFinalize = false;

    const first = await policy.refund(
      signedRefundRequest('execute', 'job-retry-refund', facts.signature),
      'logical-refund-retry',
    );
    expect(first.status).toBe('submitted');
    chain.states.set(first.transactionSignature!, 'failed');
    chain.autoFinalize = true;

    const second = await policy.refund(
      signedRefundRequest('execute', 'job-retry-refund', facts.signature),
      'logical-refund-retry',
    );
    expect(second.status).toBe('finalized');
    expect(second.id).not.toBe(first.id);
    const commandId = String(second.details.refundCommandId);
    await expect(store.getRefundCommand(commandId)).resolves.toMatchObject({
      id: commandId,
      attemptCount: 2,
      currentOperationId: second.id,
      status: 'finalized',
    });
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

  it('treats a non-retryable error after broadcast handoff as indeterminate', async () => {
    const { chain, policy } = fixture();
    const facts = settlement();
    chain.settlements.set(facts.signature, facts);
    chain.broadcast = async (prepared) => {
      if (!chain.states.has(prepared.signature)) {
        chain.applications.set(prepared.signature, 1);
        chain.states.set(prepared.signature, 'finalized');
      }
      throw new PolicyError('transaction_rejected', 'provider closed after send', 422);
    };

    const first = await registerAndRefund(
      policy,
      'job-post-send-error',
      facts.signature,
      'refund-post-send-error',
    );
    expect(first).toMatchObject({
      status: 'reconciling',
      errorCode: 'broadcast_indeterminate',
    });

    await expect(policy.drive(first.id)).resolves.toMatchObject({ status: 'finalized' });
    expect(chain.applications.get(first.transactionSignature!)).toBe(1);
  });

  it('reconciles legacy signatures before parking records that lost signed bytes', async () => {
    const outcomes = {
      finalized: { status: 'finalized', errorCode: null },
      failed: { status: 'rejected', errorCode: 'transaction_failed' },
      submitted: { status: 'submitted', errorCode: null },
      missing: { status: 'reconciling', errorCode: 'signed_transaction_missing' },
    } as const;

    for (const [state, expected] of Object.entries(outcomes)) {
      const { chain, policy, store } = fixture();
      const facts = settlement(state[0]!.toUpperCase().repeat(64));
      chain.settlements.set(facts.signature, facts);
      chain.autoFinalize = false;
      const operation = await registerAndRefund(
        policy,
        `job-legacy-${state}`,
        facts.signature,
        `refund-legacy-${state}`,
      );
      const signature = operation.transactionSignature!;
      const owner = `legacy-fixture-${state}`;
      const leased = await store.acquireLease(operation.id, owner, new Date(), 5_000);
      expect(leased).not.toBeNull();
      await store.update(operation.id, owner, leased!.version, {
        status: 'reconciling',
        prepared: null,
      });
      await store.releaseLease(operation.id, owner);
      if (state === 'missing') chain.states.delete(signature);
      else chain.states.set(signature, state as 'finalized' | 'failed' | 'submitted');

      await expect(policy.drive(operation.id)).resolves.toMatchObject({
        ...expected,
        prepared: null,
        transactionSignature: signature,
      });
      expect(chain.preparedOperations).toHaveLength(1);
    }
  });

  it('never rebuilds an expired refund whose broadcast outcome is missing', async () => {
    const { chain, policy } = fixture();
    const facts = settlement();
    chain.settlements.set(facts.signature, facts);
    chain.throwAfterBroadcastOnce = true;

    const first = await registerAndRefund(policy, 'job-expired', facts.signature, 'refund-expired');
    const prepared = structuredClone(first.prepared);
    expect(first.status).toBe('reconciling');
    chain.states.delete(first.transactionSignature!);
    chain.currentBlockHeight = first.prepared!.lastValidBlockHeight + 1;

    const recovered = await policy.drive(first.id);
    expect(recovered).toMatchObject({
      status: 'reconciling',
      errorCode: 'transaction_outcome_indeterminate',
      transactionSignature: first.transactionSignature,
      prepared,
    });
    expect(chain.preparedOperations).toHaveLength(1);
    expect([...chain.applications.keys()]).toEqual([first.transactionSignature]);

    const retried = await policy.drive(first.id);
    expect(retried).toMatchObject({
      status: 'reconciling',
      errorCode: 'transaction_outcome_indeterminate',
      transactionSignature: first.transactionSignature,
      prepared,
    });
    expect(chain.preparedOperations).toHaveLength(1);
    expect([...chain.applications.keys()]).toEqual([first.transactionSignature]);
  });

  it('does not reject a prior broadcast when exact rebroadcast preflight is inconclusive', async () => {
    const { chain, policy } = fixture();
    const facts = settlement();
    chain.settlements.set(facts.signature, facts);
    chain.throwAfterBroadcastOnce = true;

    const first = await registerAndRefund(
      policy,
      'job-preflight',
      facts.signature,
      'refund-preflight',
    );
    chain.states.delete(first.transactionSignature!);
    chain.broadcast = async () => {
      throw new PolicyError('transaction_preflight_failed', 'state changed', 409);
    };

    const recovered = await policy.drive(first.id);
    expect(recovered).toMatchObject({
      status: 'reconciling',
      errorCode: 'broadcast_indeterminate',
      transactionSignature: first.transactionSignature,
      prepared: first.prepared,
    });
    expect(chain.preparedOperations).toHaveLength(1);
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
    const context = fixture();
    const { chain } = context;
    const reserve = await createProtectedEscrow(context, escrowRequest('bounty-1'), 'escrow-key-1');

    expect(reserve.status).toBe('finalized');
    expect(reserve.recipient).toBe('escrow-vault');
    expect(operationView(reserve)).toMatchObject({
      amountAtomic: '100000000',
    });
    expect(reserve.details).toMatchObject({
      escrowAddress: expect.any(String),
      vaultAddress: expect.any(String),
    });
    expect(
      chain.preparedOperations.find((operation) => operation.kind === 'escrow_reserve'),
    ).toEqual(expect.objectContaining({ kind: 'escrow_reserve', amountLamports: '100000000' }));
  });

  it('separates protected refund capacity from the escrow spending cap', async () => {
    const context = fixture({ refundDailyLimit: 200, escrowDailyLimit: 1_000 });
    const { policy } = context;

    await expect(
      createProtectedEscrow(context, escrowRequest('bounty-cap'), 'escrow-cap'),
    ).resolves.toMatchObject({ status: 'finalized' });
    await expect(policy.readiness()).resolves.toMatchObject({
      remainingRefundLimitUsdCents: 0,
      escrowRollingLimitUsdCents: 1_000,
      rollingEscrowSpendUsdCents: 1_000,
      remainingEscrowLimitUsdCents: 0,
    });
  });

  it('rejects reserve value above either the operation or absolute asset ceiling', async () => {
    const overOperation = fixture();
    const { acceptanceHash: _acceptanceHash, ...overOperationTerms } =
      escrowRequest('bounty-operation');
    const overOperationRequest = {
      ...overOperationTerms,
      amountUsdCents: 2_501,
    };
    await expect(
      createProtectedEscrow(
        overOperation,
        {
          ...overOperationRequest,
          acceptanceHash: escrowAcceptanceHash(overOperationRequest),
        },
        'escrow-operation',
      ),
    ).rejects.toMatchObject({ code: 'operation_limit_exceeded' });

    const overAsset = fixture({ maxEscrowLamports: 50_000_000 });
    await expect(
      createProtectedEscrow(overAsset, escrowRequest('bounty-asset'), 'escrow-asset'),
    ).rejects.toMatchObject({ code: 'escrow_asset_limit_exceeded' });
  });

  it('rejects a reserve when SOL cannot cover principal, both rents, and fee reserve', async () => {
    const context = fixture();
    const { chain, policy } = context;
    const request = escrowRequest('bounty-capacity');
    await protectBountySource(context, request);
    chain.escrowLamports = 102_999_999n;
    await expect(policy.createEscrow(request, 'escrow-capacity')).rejects.toMatchObject({
      code: 'escrow_pool_insufficient',
    });
  });

  it('requires a signer-issued challenge and a valid claimant wallet signature', async () => {
    const context = fixture();
    const { policy } = context;
    const reserve = await createProtectedEscrow(
      context,
      escrowRequest('bounty-bind'),
      'reserve-bind',
    );
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
    const context = fixture();
    const { policy } = context;
    const reserve = await createProtectedEscrow(
      context,
      escrowRequest('bounty-bind-race'),
      'reserve-race',
    );
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
    const context = fixture();
    const { chain, merges, policy } = context;
    const reserve = await createProtectedEscrow(
      context,
      escrowRequest('bounty-login'),
      'reserve-login',
    );
    const preparedBefore = chain.preparedOperations.length;
    merges.error = new PolicyError('github_identity_invalid', 'missing', 422);

    await expect(
      policy.issueGitHubIdentityGrant({ accessToken: 'o'.repeat(20) }),
    ).rejects.toMatchObject({ code: 'github_identity_invalid' });
    expect(chain.preparedOperations).toHaveLength(preparedBefore);
  });

  it('consumes a GitHub identity grant exactly once', async () => {
    const context = fixture();
    const { policy } = context;
    const first = await createProtectedEscrow(
      context,
      escrowRequest('bounty-grant-a'),
      'reserve-grant-a',
    );
    const second = await createProtectedEscrow(
      context,
      escrowRequest('bounty-grant-b'),
      'reserve-grant-b',
    );
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
    const context = fixture();
    const { policy, store, merges } = context;
    const { reserve, binding } = await reserveAndBind(context, 'bounty-evidence');
    const release = await policy.releaseEscrow(
      reserve.id,
      releaseRequest(reserve.id, binding.createdAt.toISOString()),
      'release-evidence',
    );
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
    const context = fixture();
    const { policy, merges } = context;
    const { reserve, binding } = await reserveAndBind(context, 'bounty-release-replay');
    const input = releaseRequest(reserve.id, binding.createdAt.toISOString());
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

  it('permits one paid review attempt while preserving the expiry refund path', async () => {
    let now = new Date('2026-08-22T12:00:00.000Z');
    const context = fixture({ now: () => new Date(now) });
    const { policy, reviewer } = context;
    const { reserve, binding, challenge } = await reserveAndBind(context, 'bounty-review-attempt');
    reviewer.error = new PolicyError(
      'independent_review_rejected',
      'Independent review rejected the release',
      422,
    );
    const input = releaseRequest(reserve.id, binding.createdAt.toISOString(), now);

    await expect(
      policy.releaseEscrow(reserve.id, input, 'release-review-attempt-one'),
    ).resolves.toMatchObject({
      status: 'rejected',
      errorCode: 'independent_review_rejected',
    });
    await expect(
      policy.releaseEscrow(reserve.id, input, 'release-review-attempt-two'),
    ).rejects.toMatchObject({ code: 'resource_conflict' });
    expect(reviewer.requests).toHaveLength(1);

    now = new Date(challenge.claimExpiresAt);
    await expect(
      policy.refundEscrow(reserve.id, { reasonCode: 'rejected' }, 'refund-review-rejection'),
    ).resolves.toMatchObject({ kind: 'escrow_refund', status: 'finalized' });
    expect(reviewer.requests).toHaveLength(1);
  });

  it('does not release without a finalized claimant binding', async () => {
    const context = fixture();
    const { policy } = context;
    const reserve = await createProtectedEscrow(
      context,
      escrowRequest('bounty-unbound'),
      'reserve-unbound',
    );
    await expect(
      policy.releaseEscrow(
        reserve.id,
        releaseRequest(reserve.id, reserve.createdAt.toISOString()),
        'release-unbound',
      ),
    ).rejects.toMatchObject({ code: 'escrow_not_bound' });
  });

  it('uses offer expiry for unbound refunds at the exact boundary', async () => {
    let now = new Date('2026-08-22T12:00:00.000Z');
    const context = fixture({ now: () => new Date(now) });
    const { chain, policy } = context;
    const reserve = await createProtectedEscrow(
      context,
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
    const context = fixture({ now: () => new Date(now) });
    const { policy } = context;
    const { reserve, challenge } = await reserveAndBind(
      context,
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
    const context = fixture({ now: () => new Date(now) });
    const { policy } = context;
    const { reserve, challenge, binding } = await reserveAndBind(context, 'bounty-resolution-race');
    now = new Date(challenge.claimExpiresAt);
    const outcomes = await Promise.allSettled([
      policy.releaseEscrow(
        reserve.id,
        releaseRequest(reserve.id, binding.createdAt.toISOString(), now),
        'release-race',
      ),
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
    const context = fixture();
    const { merges, policy } = context;
    const { reserve, challenge, binding } = await reserveAndBind(context, 'bounty-late-merge');
    const original = merges.verify.bind(merges);
    merges.verify = async (request) => {
      const verified = await original(request);
      return {
        ...verified,
        evidence: {
          ...verified.evidence,
          mergedAt: new Date(Date.parse(challenge.claimExpiresAt) + 1).toISOString(),
        },
      };
    };

    await expect(
      policy.releaseEscrow(
        reserve.id,
        releaseRequest(reserve.id, binding.createdAt.toISOString()),
        'release-late-merge',
      ),
    ).rejects.toMatchObject({ code: 'github_merge_after_expiry' });
  });

  it('locks an expired missing release transaction against a competing refund', async () => {
    let now = new Date('2026-08-22T12:00:00.000Z');
    const context = fixture({ now: () => new Date(now) });
    const { chain, policy } = context;
    const { reserve, challenge, binding } = await reserveAndBind(context, 'bounty-release-handoff');
    chain.autoFinalize = false;
    const release = await policy.releaseEscrow(
      reserve.id,
      releaseRequest(reserve.id, binding.createdAt.toISOString(), now),
      'release-handoff',
    );
    expect(release.status).toBe('submitted');
    chain.states.delete(release.transactionSignature!);
    chain.currentBlockHeight = release.prepared!.lastValidBlockHeight + 1;
    now = new Date(challenge.claimExpiresAt);

    const expired = await policy.drive(release.id);
    expect(expired).toMatchObject({
      status: 'reconciling',
      errorCode: 'transaction_outcome_indeterminate',
      transactionSignature: release.transactionSignature,
      prepared: release.prepared,
    });
    expect(chain.preparedOperations.filter(({ kind }) => kind === 'escrow_release')).toHaveLength(
      1,
    );
    chain.autoFinalize = true;
    await expect(
      policy.refundEscrow(reserve.id, { reasonCode: 'expired' }, 'refund-after-handoff'),
    ).rejects.toMatchObject({ code: 'resource_conflict' });
  });

  it('does not let a permanent prepare failure occupy the resolution resource forever', async () => {
    let now = new Date('2026-08-22T12:00:00.000Z');
    const context = fixture({ now: () => new Date(now) });
    const { chain, policy } = context;
    const { reserve, challenge, binding } = await reserveAndBind(
      context,
      'bounty-prepare-rejection',
    );
    const originalPrepare = chain.prepare.bind(chain);
    chain.prepare = async (operation) => {
      if (operation.kind === 'escrow_release') {
        throw new PolicyError('transaction_form_not_allowed', 'invalid release form', 403);
      }
      return originalPrepare(operation);
    };
    const release = await policy.releaseEscrow(
      reserve.id,
      releaseRequest(reserve.id, binding.createdAt.toISOString(), now),
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
    const context = fixture();
    const { policy } = context;
    const request = escrowRequest('bounty-tombstone');
    await createProtectedEscrow(context, request, 'reserve-first');
    await expect(policy.createEscrow(request, 'reserve-second')).rejects.toMatchObject({
      code: 'resource_conflict',
    });
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
  overrides: { sourceJobId?: string; amountUsdCents?: number } = {},
) {
  const request = {
    bountyId,
    sourceJobId: overrides.sourceJobId ?? `job-${bountyId}`,
    amountUsdCents: 1_000,
    expiresAt,
    repository: 'owner/repository',
    issueNumber: 17,
    issueTitle: 'Handle empty input',
    issueBody: 'The parser should accept an empty input.',
    baseRef: 'main',
    baseSha: 'd'.repeat(40),
    reviewPolicy: { version: 1 as const, model: 'independent-reviewer', maxFiles: 3 },
    ...(overrides.amountUsdCents === undefined ? {} : { amountUsdCents: overrides.amountUsdCents }),
  };
  return { ...request, acceptanceHash: escrowAcceptanceHash(request) };
}

async function createProtectedEscrow(
  context: ReturnType<typeof fixture>,
  request: ReturnType<typeof escrowRequest>,
  idempotencyKey: string,
) {
  await protectBountySource(context, request);
  return context.policy.createEscrow(request, idempotencyKey);
}

async function reserveAndBind(
  context: ReturnType<typeof fixture>,
  bountyId: string,
  offerExpiresAt?: string,
) {
  const reserve = await createProtectedEscrow(
    context,
    escrowRequest(bountyId, offerExpiresAt),
    `reserve-${bountyId}`,
  );
  const grant = await context.policy.issueGitHubIdentityGrant({ accessToken: 'o'.repeat(20) });
  const challenge = await context.policy.issueBindChallenge(reserve.id, {
    claimantWallet: CLAIMANT,
    githubGrantId: grant.id,
  });
  const binding = await context.policy.bindEscrow(
    reserve.id,
    { challengeId: challenge.id, signature: signChallenge(challenge.message) },
    `bind-${bountyId}`,
  );
  return { reserve, challenge, binding };
}

let bountySourceSequence = 0;

async function protectBountySource(
  context: ReturnType<typeof fixture>,
  request: ReturnType<typeof escrowRequest>,
): Promise<void> {
  const quoteId = randomUUID();
  const admissionId = randomUUID();
  const authorization = paymentAuthorization(quoteId);
  const identity = authorizedSettlementTransaction({
    wireTransaction: authorization.wireTransaction,
    feePayer: authorization.feePayer,
    rawAmount: '2000000',
    notBeforeUnixSeconds: 0,
  });
  const admittedAt = context.now();
  const admissionIdentity = {
    quoteId,
    repository: request.repository,
    issueNumber: request.issueNumber,
    baseRef: request.baseRef,
    baseSha: request.baseSha,
    reservationKeyHash: createHash('sha256').update(`reservation:${quoteId}`).digest('hex'),
    paymentAuthorizationHash: createHash('sha256').update(authorization.header).digest('hex'),
  };
  const binding = {
    settlementMessageHash: identity.messageHash,
    settlementClientSignature: identity.clientSignature,
    settlementFeePayer: identity.feePayer,
    settlementPayer: identity.payer,
    settlementMemo: identity.memo,
    settlementRawAmount: '2000000',
    paymentWindowStartUnixSeconds: Math.floor(admittedAt.getTime() / 1_000) - 30,
    paymentWindowEndUnixSeconds: Math.floor(admittedAt.getTime() / 1_000) + 330,
  };
  const evidence = {
    version: 2,
    ...admissionIdentity,
    ...binding,
    verifierAppId: '12345',
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
    tokenRepositories: 1,
    tokenExpiresAt: new Date(admittedAt.getTime() + 60 * 60_000).toISOString(),
    admittedAt: admittedAt.toISOString(),
  };
  const admission: RepositoryAdmission = {
    id: admissionId,
    idempotencyKey: `admission-${admissionId}`,
    requestHash: requestHash(admissionIdentity),
    ...admissionIdentity,
    ...binding,
    verifierAppId: evidence.verifierAppId,
    installationId: evidence.installationId,
    repositorySelection: evidence.repositorySelection,
    permissions: evidence.permissions,
    tokenRepositories: evidence.tokenRepositories,
    tokenExpiresAt: new Date(evidence.tokenExpiresAt),
    admittedAt,
    evidenceHash: requestHash(evidence),
  };
  await context.store.registerRepositoryAdmission(admission);
  const unsigned = {
    jobId: request.sourceJobId,
    repositoryAdmissionId: admission.id,
    repositoryAdmissionEvidenceHash: admission.evidenceHash,
    repository: request.repository,
    issueNumber: request.issueNumber,
    baseRef: request.baseRef,
    baseSha: request.baseSha,
    repositoryAuthorizedAt: admission.admittedAt.toISOString(),
    authorizationEvidenceHash: createHash('sha256')
      .update(`authorization:${request.sourceJobId}`)
      .digest('hex'),
    bountyAmountUsdCents: request.amountUsdCents,
    authorizationExpiresAt: new Date(context.now().getTime() + 10 * 60_000).toISOString(),
  };
  const intent = await context.policy.createPaymentIntent(
    {
      ...unsigned,
      authorizationSignature: signWithKey(
        JOB_AUTHORITY,
        paymentIntentAuthorizationMessage(unsigned),
      ),
    },
    `intent-${request.sourceJobId}`,
  );
  const signatureAlphabet = '23456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz';
  const signature =
    signatureAlphabet[bountySourceSequence++ % signatureAlphabet.length]!.repeat(64);
  context.chain.settlements.set(signature, {
    ...settlement(signature),
    payer: admission.settlementPayer!,
    blockTimeUnixSeconds: Math.floor(admittedAt.getTime() / 1_000),
  });
  await context.policy.activatePaymentIntent(
    intent.id,
    { settlementSignature: signature },
    `activate-${request.sourceJobId}`,
  );
  await context.policy.refund(
    signedRefundRequest(
      'execute',
      request.sourceJobId,
      signature,
      new Date(context.now().getTime() + 10 * 60_000).toISOString(),
    ),
    `refund-${request.sourceJobId}`,
  );
}

function signChallenge(message: string): string {
  return signWithKey(CLAIMANT_KEYPAIR, message);
}

function signedPaymentIntentRequest(
  jobId: string,
  authorizationExpiresAt = new Date(Date.now() + 10 * 60 * 1_000).toISOString(),
): CreatePaymentIntentRequest {
  const unsigned = {
    jobId,
    repositoryAdmissionId: DEFAULT_ADMISSION_ID,
    repositoryAdmissionEvidenceHash: defaultAdmissionEvidenceHash,
    repository: 'owner/repository',
    issueNumber: 17,
    baseRef: 'main',
    baseSha: 'd'.repeat(40),
    repositoryAuthorizedAt: '2026-08-22T11:00:00.000Z',
    authorizationEvidenceHash: 'e'.repeat(64),
    bountyAmountUsdCents: 1_000,
    authorizationExpiresAt,
  };
  return {
    ...unsigned,
    authorizationSignature: signWithKey(JOB_AUTHORITY, paymentIntentAuthorizationMessage(unsigned)),
  };
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
