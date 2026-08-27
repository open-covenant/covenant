import { describe, expect, it } from 'vitest';

import { createDemoApi } from './demo';
import type { LaunchRequest } from './domain';

const request: LaunchRequest = {
  app_id: 'gpu-workspace',
  duration_secs: 1_800,
  max_usdc_micros: 500_000,
  min_trust: 'open',
};

describe('compute simulation', () => {
  it('does not require or retain private-beta authentication', async () => {
    const api = createDemoApi();

    expect(await api.runtimeStatus()).toMatchObject({
      authentication: { source: 'none' },
      token_required: false,
    });
    expect(await api.configureSessionToken('ignored-in-demo')).toEqual({
      source: 'none',
    });
  });

  it('replays a launch for the same idempotency key', async () => {
    const api = createDemoApi();
    const first = await api.launchJob(request, 'same-launch');
    const replay = await api.launchJob(request, 'same-launch');

    expect(replay.id).toBe(first.id);
    expect(await api.listJobs()).toHaveLength(1);
  });

  it('honors the requested trust floor when quoting', async () => {
    const api = createDemoApi();
    const plan = await api.planJob(
      { ...request, min_trust: 'isolated' },
      'review-isolated',
    );

    expect(plan.offer.trust_class).toBe('isolated');
  });
});
