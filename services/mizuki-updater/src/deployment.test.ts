import { afterEach, describe, expect, it, vi } from 'vitest';
import { HttpDeploymentGateway } from './deployment.js';
import { proposalFixture } from './test-utils.js';

describe('deployment hooks', () => {
  afterEach(() => vi.unstubAllGlobals());

  it('separates shadow health from production activation evidence', async () => {
    const manifest = proposalFixture().proposal.manifest;
    const mergeSha = 'b'.repeat(40);
    const requests: Array<{ url: string; init: RequestInit }> = [];
    vi.stubGlobal(
      'fetch',
      vi.fn(async (input: string | URL | Request, init: RequestInit) => {
        requests.push({ url: String(input), init });
        if (String(input).includes('/production/')) {
          return Response.json({
            status: 'healthy',
            candidateSha: manifest.candidateSha,
            environment: 'production',
            active: true,
            mergeSha,
            promotionOperationId: 'operation-1',
          });
        }
        if (String(input).includes('/shadow-health/')) {
          return Response.json({
            status: 'healthy',
            candidateSha: manifest.candidateSha,
            environment: 'shadow',
          });
        }
        if (String(input).endsWith('/shadow')) {
          return Response.json({ deploymentId: 'shadow-1' });
        }
        return Response.json({ status: 'completed', operationId: 'operation-1' });
      }),
    );
    const gateway = createGateway();
    await expect(
      gateway.startShadow('00000000-0000-4000-8000-000000000001', manifest, 'f'.repeat(64), 42),
    ).resolves.toEqual({ deploymentId: 'shadow-1' });
    await expect(gateway.shadowHealth('shadow-1', manifest.candidateSha)).resolves.toEqual({
      status: 'healthy',
      detail: undefined,
    });
    await expect(
      gateway.promote('00000000-0000-4000-8000-000000000001', 'shadow-1', manifest, mergeSha),
    ).resolves.toEqual({ operationId: 'operation-1' });
    await expect(
      gateway.promotionHealth('shadow-1', manifest.candidateSha, mergeSha, 'operation-1'),
    ).resolves.toMatchObject({
      status: 'healthy',
      active: true,
      mergeSha,
      operationId: 'operation-1',
    });
    await gateway.finalize(
      '00000000-0000-4000-8000-000000000001',
      'shadow-1',
      manifest,
      mergeSha,
      'operation-1',
    );
    await gateway.rollback(
      '00000000-0000-4000-8000-000000000001',
      'shadow-1',
      manifest,
      'test_failure',
      'operation-1',
    );

    expect(requests[0].init.headers).toMatchObject({
      authorization: `Bearer ${'d'.repeat(32)}`,
      'idempotency-key': '00000000-0000-4000-8000-000000000001:shadow',
    });
    expect(requests[2].init.headers).toMatchObject({
      'idempotency-key': '00000000-0000-4000-8000-000000000001:promote',
    });
    expect(requests[4].init.headers).toMatchObject({
      'idempotency-key': '00000000-0000-4000-8000-000000000001:finalize',
    });
    expect(requests[5].init.headers).toMatchObject({
      'idempotency-key': '00000000-0000-4000-8000-000000000001:rollback',
    });
    expect(JSON.parse(String(requests[5].init.body))).toMatchObject({
      promotionOperationId: 'operation-1',
    });
  });

  it('rejects a health receipt for another commit', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn(async () =>
        Response.json({
          status: 'healthy',
          candidateSha: 'c'.repeat(40),
          environment: 'shadow',
        }),
      ),
    );
    await expect(createGateway().shadowHealth('shadow-1', 'a'.repeat(40))).rejects.toMatchObject({
      code: 'health_commit_mismatch',
    });
  });

  it('rejects production health that is not active or bound to the promotion', async () => {
    const manifest = proposalFixture().proposal.manifest;
    const mergeSha = 'b'.repeat(40);
    let payload = productionHealth(manifest.candidateSha, mergeSha, {
      active: false,
    });
    vi.stubGlobal(
      'fetch',
      vi.fn(async () => Response.json(payload)),
    );
    await expect(
      createGateway().promotionHealth('shadow-1', manifest.candidateSha, mergeSha, 'operation-1'),
    ).rejects.toMatchObject({ code: 'promotion_not_active' });

    payload = productionHealth(manifest.candidateSha, mergeSha, {
      promotionOperationId: 'operation-2',
    });
    await expect(
      createGateway().promotionHealth('shadow-1', manifest.candidateSha, mergeSha, 'operation-1'),
    ).rejects.toMatchObject({ code: 'promotion_operation_mismatch' });
  });

  it('marks transient hook failures retryable', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn(async () => Response.json({ message: 'busy' }, { status: 503 })),
    );
    const manifest = proposalFixture().proposal.manifest;
    await expect(
      createGateway().startShadow('upgrade-1', manifest, 'f'.repeat(64), 42),
    ).rejects.toMatchObject({ code: 'deployment_request_failed', retryable: true });
  });

  it('requires an authenticated deployment-controller readiness contract', async () => {
    const request = vi.fn(async () =>
      Response.json({ status: 'ok', service: 'mizuki-deployment-controller' }),
    );
    vi.stubGlobal('fetch', request);

    await expect(createGateway().readiness()).resolves.toBeUndefined();
    expect(request).toHaveBeenCalledWith(
      'https://deploy.example.test/readyz',
      expect.objectContaining({
        headers: expect.objectContaining({ authorization: `Bearer ${'d'.repeat(32)}` }),
      }),
    );
  });
});

function createGateway(): HttpDeploymentGateway {
  return new HttpDeploymentGateway({
    readinessUrl: 'https://deploy.example.test/readyz',
    shadowUrl: 'https://deploy.example.test/shadow',
    shadowHealthUrlTemplate: 'https://deploy.example.test/shadow-health/{deploymentId}/health',
    promotionHealthUrlTemplate: 'https://deploy.example.test/production/{deploymentId}/health',
    promoteUrl: 'https://deploy.example.test/promote',
    finalizeUrl: 'https://deploy.example.test/finalize',
    rollbackUrl: 'https://deploy.example.test/rollback',
    token: 'd'.repeat(32),
    timeoutMs: 1_000,
  });
}

function productionHealth(
  candidateSha: string,
  mergeSha: string,
  patch: Partial<{
    active: boolean;
    promotionOperationId: string;
  }> = {},
) {
  return {
    status: 'healthy',
    candidateSha,
    environment: 'production',
    active: true,
    mergeSha,
    promotionOperationId: 'operation-1',
    ...patch,
  };
}
