import type { AddressInfo } from 'node:net';
import { afterEach, beforeEach, describe, expect, it } from 'vitest';
import type { DeploymentGateway } from './deployment.js';
import type { UpgradeManifest } from './domain.js';
import type { GitHubGateway } from './github.js';
import { UpdaterMetrics } from './metrics.js';
import { createUpdaterServer } from './server.js';
import { InMemoryUpgradeRepository } from './store.js';
import { proposalFixture } from './test-utils.js';
import { UpdaterService } from './updater.js';
import type { ArtifactVerifier } from './verification.js';
import { ProposalVerifier } from './verification.js';

const SUBMIT_TOKEN = 'test-submit-token-with-32-characters';
const CONTROL_TOKEN = 'test-control-token-with-32-characters';
const READ_TOKEN = 'test-read-only-token-with-32-characters';

describe('updater HTTP service', () => {
  let server: ReturnType<typeof createUpdaterServer>;
  let origin: string;
  let proposal: ReturnType<typeof proposalFixture>['proposal'];

  beforeEach(async () => {
    const fixture = proposalFixture();
    proposal = fixture.proposal;
    const repository = new InMemoryUpgradeRepository();
    const metrics = new UpdaterMetrics();
    const service = new UpdaterService(
      {
        checkTimeoutMs: 60_000,
        healthTimeoutMs: 60_000,
        promotionSoakMs: 60_000,
        promotionTimeoutMs: 5 * 60_000,
        pollIntervalMs: 10_000,
        leaseMs: 30_000,
        maxAttempts: 3,
      },
      repository,
      new ProposalVerifier({
        trustedProposalKeys: { 'release-key-1': fixture.publicKey },
        trustedBenchmarkKeys: { 'benchmark-key-1': fixture.benchmarkPublicKey },
        trustedReviewKeys: { 'review-key-1': fixture.reviewPublicKey },
        allowedRepositories: new Set(['mizuki-labs/mizuki']),
        allowedBaseBranches: new Set(['main']),
        headBranchPrefix: 'mizuki/',
        mandatoryChecks: new Set(['test', 'security']),
        maxProposalAgeMs: 7 * 24 * 60 * 60_000,
      }),
      new PassingArtifactVerifier(),
      new PendingGitHub(),
      new IdleDeployment(),
      metrics,
      () => new Date('2026-08-22T12:00:00.000Z'),
    );
    server = createUpdaterServer({
      service,
      repository,
      metrics,
      submitToken: SUBMIT_TOKEN,
      controlToken: CONTROL_TOKEN,
      readToken: READ_TOKEN,
    });
    await new Promise<void>((resolve) => server.listen(0, '127.0.0.1', resolve));
    origin = `http://127.0.0.1:${(server.address() as AddressInfo).port}`;
  });

  afterEach(async () => {
    await new Promise<void>((resolve) => server.close(() => resolve()));
  });

  it('exposes health without exposing protected operations', async () => {
    const health = await fetch(`${origin}/health`);
    expect(health.status).toBe(200);
    expect(await health.json()).toEqual({ status: 'ok', service: 'mizuki-updater' });

    const ready = await fetch(`${origin}/readyz`);
    expect(ready.status).toBe(200);
    expect(await ready.json()).toMatchObject({
      ready: true,
      failed: [],
      dependencies: { postgres: { ok: true }, operational: { ok: true } },
    });

    const protectedResponse = await fetch(
      `${origin}/v1/upgrades/00000000-0000-4000-8000-000000000000`,
    );
    expect(protectedResponse.status).toBe(401);
    expect(await protectedResponse.json()).toMatchObject({ error: { code: 'unauthorized' } });
  });

  it('boots closed without operational secrets and rejects every authority-opening path', async () => {
    await new Promise<void>((resolve) => server.close(() => resolve()));
    const repository = new InMemoryUpgradeRepository();
    server = createUpdaterServer({
      repository,
      metrics: new UpdaterMetrics(),
      submitToken: SUBMIT_TOKEN,
      controlToken: CONTROL_TOKEN,
      readToken: READ_TOKEN,
      operationalFailures: [
        'MIZUKI_UPDATER_GITHUB_PRIVATE_KEY',
        'MIZUKI_UPDATER_DEPLOY_HOOK_TOKEN',
      ],
    });
    await new Promise<void>((resolve) => server.listen(0, '127.0.0.1', resolve));
    origin = `http://127.0.0.1:${(server.address() as AddressInfo).port}`;

    const health = await fetch(`${origin}/health`);
    expect(health.status).toBe(200);
    const ready = await fetch(`${origin}/readyz`);
    expect(ready.status).toBe(503);
    const report = await ready.json();
    expect(report).toMatchObject({
      ready: false,
      failed: ['operational'],
      dependencies: {
        operational: {
          ok: false,
          configurationIssues: [
            'MIZUKI_UPDATER_GITHUB_PRIVATE_KEY',
            'MIZUKI_UPDATER_DEPLOY_HOOK_TOKEN',
          ],
        },
      },
    });

    const submitted = await submit(origin, proposal, 'closed-updater-key');
    expect(submitted.response.status).toBe(503);
    expect(submitted.body).toMatchObject({ error: { code: 'updater_not_ready' } });

    const enabled = await updatePromotionControl(origin, CONTROL_TOKEN, 0, true);
    expect(enabled.status).toBe(503);
    await expect(repository.promotionControl()).resolves.toMatchObject({
      promotionsEnabled: false,
      revision: 0,
    });
    expect(JSON.stringify(report)).not.toContain('d'.repeat(32));
  });

  it('exposes a closed promotion control and reserves mutation for write authority', async () => {
    const initial = await fetch(`${origin}/v1/admin/promotion-control`, {
      headers: { authorization: `Bearer ${READ_TOKEN}` },
    });
    expect(initial.status).toBe(200);
    expect(await initial.json()).toMatchObject({
      control: {
        promotionsEnabled: false,
        revision: 0,
        reason: 'promotions are closed until explicitly enabled',
        updatedBy: 'system',
      },
    });

    const readOnly = await updatePromotionControl(origin, READ_TOKEN, 0, true);
    expect(readOnly.status).toBe(401);

    const enabled = await updatePromotionControl(origin, CONTROL_TOKEN, 0, true);
    expect(enabled.status).toBe(200);
    expect(await enabled.json()).toMatchObject({
      control: {
        promotionsEnabled: true,
        revision: 1,
        reason: 'controlled canary promotion',
        updatedBy: expect.stringMatching(/^control:[a-f0-9]{16}$/),
      },
    });

    const stale = await updatePromotionControl(origin, CONTROL_TOKEN, 0, false);
    expect(stale.status).toBe(409);
    expect(await stale.json()).toMatchObject({
      error: { code: 'promotion_control_conflict' },
    });
  });

  it('accepts a valid signed proposal and replays it idempotently', async () => {
    const first = await submit(origin, proposal, 'proposal-key-1');
    expect(first.response.status).toBe(202);
    const id = String(first.body.upgrade.id);

    const replay = await submit(origin, proposal, 'proposal-key-1');
    expect(replay.response.status).toBe(202);
    expect(replay.body.upgrade.id).toBe(id);

    const status = await fetch(`${origin}/v1/upgrades/${id}`, {
      headers: { authorization: `Bearer ${READ_TOKEN}` },
    });
    expect(status.status).toBe(200);
    expect(await status.json()).toMatchObject({
      upgrade: {
        id,
        proposalId: 'upgrade-1',
        sourceHandoffSha256: 'f'.repeat(64),
        attestations: {
          proposal: { keyId: 'release-key-1' },
          benchmark: { receiptId: 'benchmark-1', keyId: 'benchmark-key-1' },
          review: { receiptId: 'review-1', keyId: 'review-key-1' },
        },
      },
    });

    const byProposal = await fetch(`${origin}/v1/proposals/upgrade-1`, {
      headers: { authorization: `Bearer ${READ_TOKEN}` },
    });
    expect(byProposal.status).toBe(200);
    expect(await byProposal.json()).toMatchObject({
      upgrade: {
        id,
        proposalId: 'upgrade-1',
        sourceHandoffSha256: 'f'.repeat(64),
      },
      auditHeadHash: expect.stringMatching(/^[a-f0-9]{64}$/),
    });

    const audit = await fetch(`${origin}/v1/upgrades/${id}/audit`, {
      headers: { authorization: `Bearer ${READ_TOKEN}` },
    });
    expect(audit.status).toBe(200);
    expect((await audit.json()).receipts.length).toBeGreaterThan(0);
  });

  it('rejects unknown fields and missing idempotency keys', async () => {
    const extra = { ...proposal, unexpected: true };
    const invalid = await submit(origin, extra, 'proposal-key-1');
    expect(invalid.response.status).toBe(400);
    expect(invalid.body).toMatchObject({ error: { code: 'invalid_request' } });

    const missingKey = await fetch(`${origin}/v1/upgrades`, {
      method: 'POST',
      headers: {
        authorization: `Bearer ${SUBMIT_TOKEN}`,
        'content-type': 'application/json',
      },
      body: JSON.stringify(proposal),
    });
    expect(missingKey.status).toBe(400);

    const missingHandoff = structuredClone(proposal) as Record<string, unknown>;
    delete (missingHandoff.manifest as Record<string, unknown>).sourceHandoffSha256;
    const unbound = await submit(origin, missingHandoff, 'proposal-key-unbound');
    expect(unbound.response.status).toBe(400);
    expect(unbound.body).toMatchObject({ error: { code: 'invalid_request' } });
  });

  it('protects Prometheus metrics with bearer authentication', async () => {
    expect((await fetch(`${origin}/metrics`)).status).toBe(401);
    const response = await fetch(`${origin}/metrics`, {
      headers: { authorization: `Bearer ${READ_TOKEN}` },
    });
    expect(response.status).toBe(200);
    expect(await response.text()).toContain('mizuki_updater_upgrades_total');
  });

  it('returns not found for a proposal that has not been submitted', async () => {
    const response = await fetch(`${origin}/v1/proposals/missing-proposal`, {
      headers: { authorization: `Bearer ${READ_TOKEN}` },
    });
    expect(response.status).toBe(404);
    expect(await response.json()).toMatchObject({ error: { code: 'upgrade_not_found' } });
  });

  it('does not let the read-only credential submit a proposal', async () => {
    const response = await fetch(`${origin}/v1/upgrades`, {
      method: 'POST',
      headers: {
        authorization: `Bearer ${READ_TOKEN}`,
        'content-type': 'application/json',
        'idempotency-key': 'proposal-key-1',
      },
      body: JSON.stringify(proposal),
    });
    expect(response.status).toBe(401);
  });

  it('rejects every cross-role credential substitution', async () => {
    const controlAsSubmit = await submitWithToken(
      origin,
      proposal,
      'cross-role-control',
      CONTROL_TOKEN,
    );
    expect(controlAsSubmit.status).toBe(401);

    const submitAsControl = await updatePromotionControl(origin, SUBMIT_TOKEN, 0, true);
    expect(submitAsControl.status).toBe(401);

    for (const token of [SUBMIT_TOKEN, CONTROL_TOKEN]) {
      const response = await fetch(`${origin}/v1/admin/promotion-control/audit`, {
        headers: { authorization: `Bearer ${token}` },
      });
      expect(response.status).toBe(401);
    }
  });
});

async function submit(origin: string, body: unknown, idempotencyKey: string) {
  const response = await submitWithToken(origin, body, idempotencyKey, SUBMIT_TOKEN);
  return {
    response,
    body: (await response.json()) as {
      upgrade: { id: string; proposalId: string };
      error?: { code: string; message: string };
    },
  };
}

function submitWithToken(origin: string, body: unknown, idempotencyKey: string, token: string) {
  return fetch(`${origin}/v1/upgrades`, {
    method: 'POST',
    headers: {
      authorization: `Bearer ${token}`,
      'content-type': 'application/json',
      'idempotency-key': idempotencyKey,
    },
    body: JSON.stringify(body),
  });
}

function updatePromotionControl(
  origin: string,
  token: string,
  expectedRevision: number,
  promotionsEnabled: boolean,
) {
  return fetch(`${origin}/v1/admin/promotion-control`, {
    method: 'PUT',
    headers: {
      authorization: `Bearer ${token}`,
      'content-type': 'application/json',
    },
    body: JSON.stringify({
      promotionsEnabled,
      expectedRevision,
      reason: promotionsEnabled ? 'controlled canary promotion' : 'incident response pause',
    }),
  });
}

class PassingArtifactVerifier implements ArtifactVerifier {
  async verify(_url: string, sha256: string, sizeBytes: number) {
    return { sha256, sizeBytes };
  }
}

class PendingGitHub implements GitHubGateway {
  async syncPullRequest() {
    return { number: 42, url: 'https://github.com/mizuki-labs/mizuki/pull/42' };
  }

  async requiredChecks() {
    return { status: 'pending' as const, checks: { test: 'pending', security: 'missing' } };
  }

  async mergeState() {
    return { status: 'open' as const };
  }

  async merge() {
    throw new Error('Merge must not run');
  }
}

class IdleDeployment implements DeploymentGateway {
  async startShadow(_id: string, _manifest: UpgradeManifest) {
    throw new Error('Shadow must not start');
  }

  async shadowHealth() {
    throw new Error('Shadow health must not run');
  }

  async promotionHealth() {
    throw new Error('Promotion health must not run');
  }

  async promote() {
    throw new Error('Promotion must not run');
  }

  async finalize() {
    throw new Error('Finalize must not run');
  }

  async rollback() {
    throw new Error('Rollback must not run');
  }
}
