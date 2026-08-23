import { createPrivateKey, sign } from 'node:crypto';
import type { AddressInfo } from 'node:net';
import { Keypair } from '@solana/web3.js';
import { afterEach, beforeEach, describe, expect, it } from 'vitest';
import { FixedUsdPriceOracle, MockChainGateway } from './chain.js';
import {
  bindEscrowRequestSchema,
  createEscrowRequestSchema,
  refundAuthorizationMessage,
  refundDischargeAuthorizationMessage,
  refundRequestSchema,
  releaseEscrowRequestSchema,
} from './domain.js';
import { SignerMetrics } from './metrics.js';
import { MockMergeVerifier } from './github.js';
import { PolicyService } from './policy.js';
import { createSignerServer } from './server.js';
import { InMemoryOperationStore } from './store.js';

const TOKEN = 'test-token-with-at-least-thirty-two-characters';
const TREASURY = '2'.repeat(32);
const MINT = '3'.repeat(32);
const SIGNATURE = '6'.repeat(64);
const CLAIMANT = Keypair.generate();
const JOB_AUTHORITY = Keypair.generate();

describe('signer HTTP service', () => {
  let server: ReturnType<typeof createSignerServer>;
  let origin: string;
  let merges: MockMergeVerifier;

  beforeEach(async () => {
    const store = new InMemoryOperationStore();
    const chain = new MockChainGateway();
    const metrics = new SignerMetrics();
    chain.settlements.set(SIGNATURE, {
      signature: SIGNATURE,
      payer: '4'.repeat(32),
      recipient: TREASURY,
      mint: MINT,
      rawAmount: '2000000',
      decimals: 6,
      finalized: true,
      succeeded: true,
      slot: 1,
      blockTimeUnixSeconds: Math.floor(Date.now() / 1_000),
    });
    merges = new MockMergeVerifier();
    const service = new PolicyService(
      {
        refundTreasury: TREASURY,
        escrowAuthority: '5'.repeat(32),
        refundMint: MINT,
        refundDecimals: 6,
        jobAuthorityPublicKey: JOB_AUTHORITY.publicKey.toBase58(),
        refundAuthMaxTtlSeconds: 900,
        refundLiabilityMaxAgeSeconds: 86_400,
        operationLimitUsdCents: 2_500,
        refundDailyLimitUsdCents: 10_000,
        escrowDailyLimitUsdCents: 10_000,
        solFeeReserveLamports: 1_000_000,
        bindChallengeTtlSeconds: 600,
        githubGrantTtlSeconds: 600,
        claimTtlSeconds: 172_800,
      },
      store,
      chain,
      new FixedUsdPriceOracle(),
      merges,
      metrics,
    );
    server = createSignerServer({ service, store, metrics, authToken: TOKEN });
    await new Promise<void>((resolve) => server.listen(0, '127.0.0.1', resolve));
    const address = server.address() as AddressInfo;
    origin = `http://127.0.0.1:${address.port}`;
  });

  afterEach(async () => {
    await new Promise<void>((resolve) => server.close(() => resolve()));
  });

  it('requires bearer authentication', async () => {
    const response = await fetch(`${origin}/v1/operations/00000000-0000-4000-8000-000000000000`);
    expect(response.status).toBe(401);
    expect(await response.json()).toMatchObject({ error: { code: 'unauthorized' } });
  });

  it('rejects caller-supplied refund recipient and amount fields', async () => {
    const response = await fetch(`${origin}/v1/refunds`, {
      method: 'POST',
      headers: {
        authorization: `Bearer ${TOKEN}`,
        'content-type': 'application/json',
        'idempotency-key': 'refund-key-1',
      },
      body: JSON.stringify({
        jobId: 'job-1',
        settlementSignature: SIGNATURE,
        payer: '9'.repeat(32),
        amount: 1,
      }),
    });
    expect(response.status).toBe(400);
    expect(await response.json()).toMatchObject({ error: { code: 'invalid_request' } });
  });

  it('returns the same operation for an authenticated retry', async () => {
    const registration = signedRefundRequest('register', 'job-1', SIGNATURE);
    const registered = await fetch(`${origin}/v1/refund-liabilities`, {
      method: 'POST',
      headers: mutationHeaders('liability-key-1'),
      body: JSON.stringify(registration),
    });
    expect(registered.status).toBe(201);
    const execution = signedRefundRequest('execute', 'job-1', SIGNATURE);
    const request = () =>
      fetch(`${origin}/v1/refunds`, {
        method: 'POST',
        headers: {
          authorization: `Bearer ${TOKEN}`,
          'content-type': 'application/json',
          'idempotency-key': 'refund-key-1',
        },
        body: JSON.stringify(execution),
      });
    const first = await request();
    const second = await request();
    const firstBody = (await first.json()) as { id: string; recipient: string };
    const secondBody = (await second.json()) as { id: string };

    expect(first.status).toBe(200);
    expect(second.status).toBe(200);
    expect(secondBody.id).toBe(firstBody.id);
    expect(firstBody.recipient).toBe('4'.repeat(32));
  });

  it('serves reserve, challenge, and signed bind as distinct finalized operations', async () => {
    const reserveResponse = await fetch(`${origin}/v1/escrows`, {
      method: 'POST',
      headers: mutationHeaders('reserve-http'),
      body: JSON.stringify({
        bountyId: 'bounty-http',
        amountUsdCents: 1_000,
        acceptanceHash: 'a'.repeat(64),
        expiresAt: new Date(Date.now() + 2 * 60 * 60 * 1_000).toISOString(),
        repository: 'owner/repository',
        issueNumber: 17,
      }),
    });
    const reserve = (await reserveResponse.json()) as Record<string, unknown>;
    expect(reserveResponse.status).toBe(200);
    expect(reserve).toMatchObject({
      kind: 'escrow_reserve',
      status: 'finalized',
      amountAtomic: '100000000',
      reservationId: reserve.id,
      bountyDigest: expect.any(String),
      escrowAddress: expect.any(String),
      vaultAddress: expect.any(String),
      guardAddress: expect.any(String),
      transactionSignature: expect.any(String),
    });

    const grantResponse = await fetch(`${origin}/v1/github/identity-grants`, {
      method: 'POST',
      headers: {
        authorization: `Bearer ${TOKEN}`,
        'content-type': 'application/json',
      },
      body: JSON.stringify({ accessToken: 'o'.repeat(20) }),
    });
    const grant = (await grantResponse.json()) as { id: string; githubId: string; login: string };
    expect(grantResponse.status).toBe(201);
    expect(grant).toMatchObject({ githubId: '42', login: 'contributor' });

    const challengeResponse = await fetch(
      `${origin}/v1/escrows/${reserve.id as string}/bind-challenges`,
      {
        method: 'POST',
        headers: {
          authorization: `Bearer ${TOKEN}`,
          'content-type': 'application/json',
        },
        body: JSON.stringify({
          claimantWallet: CLAIMANT.publicKey.toBase58(),
          githubGrantId: grant.id,
        }),
      },
    );
    const challenge = (await challengeResponse.json()) as {
      id: string;
      message: string;
      claimExpiresAt: string;
    };
    expect(challengeResponse.status).toBe(201);
    expect(Date.parse(challenge.claimExpiresAt)).toBeGreaterThan(Date.now());

    const bindResponse = await fetch(`${origin}/v1/escrows/${reserve.id as string}/bind`, {
      method: 'POST',
      headers: mutationHeaders('bind-http'),
      body: JSON.stringify({
        challengeId: challenge.id,
        signature: signMessage(challenge.message),
      }),
    });
    expect(bindResponse.status).toBe(200);
    expect(await bindResponse.json()).toMatchObject({
      kind: 'escrow_bind',
      status: 'finalized',
      amountAtomic: null,
      reservationId: reserve.id,
      recipient: CLAIMANT.publicKey.toBase58(),
    });
  });

  it('exposes dependency health and Prometheus metrics without secret data', async () => {
    const health = await fetch(`${origin}/health`);
    const metrics = await fetch(`${origin}/metrics`);

    expect(health.status).toBe(200);
    expect(await health.json()).toEqual({ ok: true });
    expect(await metrics.text()).toContain('mizuki_signer_operations_total');
  });

  it('returns authenticated dual-RPC refund admission capacity', async () => {
    const response = await fetch(`${origin}/v1/readiness`, {
      headers: { authorization: `Bearer ${TOKEN}` },
    });
    expect(response.status).toBe(200);
    expect(await response.json()).toEqual({
      healthy: true,
      refundTreasury: TREASURY,
      refundMint: MINT,
      refundDecimals: 6,
      finalizedBalanceRaw: '1000000000',
      pendingRefundRaw: '0',
      treasuryAvailableRefundRaw: '1000000000',
      remainingRefundLimitUsdCents: 10000,
      availableRefundRaw: '100000000',
      escrowAuthority: '5'.repeat(32),
      finalizedEscrowBalanceLamports: '100000000000',
      availableEscrowReserveLamports: '99994500000',
    });
  });

  it('returns authenticated production dependency evidence without secret values', async () => {
    const response = await fetch(`${origin}/v1/readiness/evidence`, {
      headers: { authorization: `Bearer ${TOKEN}` },
    });
    const body = (await response.json()) as Record<string, unknown>;

    expect(response.status).toBe(200);
    expect(body).toMatchObject({
      healthy: true,
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
        escrowAuthority: '5'.repeat(32),
      },
      prices: { feedCount: 2 },
    });
    expect(JSON.stringify(body)).not.toContain(TOKEN);
  });

  it('returns 503 from health and evidence when a required dependency is unready', async () => {
    merges.error = new Error('credential rejected');

    const [health, evidence] = await Promise.all([
      fetch(`${origin}/health`),
      fetch(`${origin}/v1/readiness/evidence`, {
        headers: { authorization: `Bearer ${TOKEN}` },
      }),
    ]);

    expect(health.status).toBe(503);
    expect(await health.json()).toEqual({ ok: false });
    expect(evidence.status).toBe(503);
    expect(await evidence.json()).toMatchObject({
      healthy: false,
      checks: { githubCredential: false },
    });
  });

  it('discharges a registered liability through independently verified merge evidence', async () => {
    const registration = await fetch(`${origin}/v1/refund-liabilities`, {
      method: 'POST',
      headers: mutationHeaders('liability-discharge-register'),
      body: JSON.stringify(signedRefundRequest('register', 'job-discharge', SIGNATURE)),
    });
    const liability = (await registration.json()) as { id: string };
    expect(registration.status).toBe(201);

    const discharge = await fetch(`${origin}/v1/refund-liabilities/${liability.id}/discharge`, {
      method: 'POST',
      headers: mutationHeaders('liability-discharge-complete'),
      body: JSON.stringify(signedDischargeRequest('job-discharge', SIGNATURE)),
    });
    expect(discharge.status).toBe(200);
    expect(await discharge.json()).toMatchObject({
      id: liability.id,
      dischargedAt: expect.any(String),
      dischargeEvidenceHash: expect.stringMatching(/^[a-f0-9]{64}$/),
    });
  });
});

describe('strict public schemas', () => {
  it('requires a short-lived job-authority signature for refunds', () => {
    const request = signedRefundRequest('execute', 'job-1', SIGNATURE);
    expect(refundRequestSchema.safeParse(request).success).toBe(true);
    expect(
      refundRequestSchema.safeParse({
        jobId: 'job-1',
        settlementSignature: SIGNATURE,
        authorizationExpiresAt: request.authorizationExpiresAt,
        authorizationSignature: request.authorizationSignature,
        recipient: '4'.repeat(32),
      }).success,
    ).toBe(false);
  });

  it('does not accept caller-derived merge receipts', () => {
    expect(releaseEscrowRequestSchema.safeParse({ pullRequestNumber: 23 }).success).toBe(true);
    expect(
      releaseEscrowRequestSchema.safeParse({
        pullRequestNumber: 23,
        mergeReceiptHash: 'a'.repeat(64),
      }).success,
    ).toBe(false);
  });

  it('forbids claimant fields during reserve and arbitrary bind fields', () => {
    expect(
      createEscrowRequestSchema.safeParse({
        bountyId: 'bounty-1',
        amountUsdCents: 1_000,
        acceptanceHash: 'a'.repeat(64),
        expiresAt: '2026-09-01T12:00:00.000Z',
        repository: 'owner/repository',
        issueNumber: 17,
        claimantWallet: CLAIMANT.publicKey.toBase58(),
      }).success,
    ).toBe(false);
    expect(
      bindEscrowRequestSchema.safeParse({
        challengeId: '00000000-0000-4000-8000-000000000000',
        signature: Buffer.alloc(64).toString('base64'),
        claimantWallet: CLAIMANT.publicKey.toBase58(),
      }).success,
    ).toBe(false);
  });
});

function mutationHeaders(idempotencyKey: string): Record<string, string> {
  return {
    authorization: `Bearer ${TOKEN}`,
    'content-type': 'application/json',
    'idempotency-key': idempotencyKey,
  };
}

function signMessage(message: string): string {
  return signWithKey(CLAIMANT, message);
}

function signedRefundRequest(
  action: 'register' | 'execute',
  jobId: string,
  settlementSignature: string,
) {
  const authorizationExpiresAt = new Date(Date.now() + 10 * 60 * 1_000).toISOString();
  const unsigned = { jobId, settlementSignature, authorizationExpiresAt };
  return {
    ...unsigned,
    authorizationSignature: signWithKey(
      JOB_AUTHORITY,
      refundAuthorizationMessage(action, unsigned),
    ),
  };
}

function signedDischargeRequest(jobId: string, settlementSignature: string) {
  const authorizationExpiresAt = new Date(Date.now() + 10 * 60 * 1_000).toISOString();
  const unsigned = {
    jobId,
    settlementSignature,
    repository: 'owner/repository',
    pullRequestNumber: 23,
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
