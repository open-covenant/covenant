import { afterEach, describe, expect, it, vi } from 'vitest';
import { HttpApplicationGateway } from './probe.js';

describe('application functional readiness probe', () => {
  afterEach(() => vi.unstubAllGlobals());

  it('probes shadow closed-state readiness without an authority token', async () => {
    const request = vi.fn(async () => Response.json({ ok: true }));
    vi.stubGlobal('fetch', request);
    await expect(createProbe().probe('srv-shadow123')).resolves.toBeUndefined();
    expect(request).toHaveBeenCalledWith(
      'http://mizuki-shadow:10000/deployz',
      expect.objectContaining({
        method: 'GET',
        headers: {
          accept: 'application/json',
          'cache-control': 'no-store',
        },
        redirect: 'error',
      }),
    );
  });

  it('requires every load-bearing production dependency check', async () => {
    const request = vi.fn(async () =>
      Response.json({
        status: 'ok',
        service: 'mizuki-api',
        checks: {
          database: 'ok',
          policySigner: 'ok',
          codingGateway: 'ok',
          settlement: 'ok',
        },
      }),
    );
    vi.stubGlobal('fetch', request);
    await expect(createProbe().probe('srv-production123')).resolves.toBeUndefined();
    expect(request).toHaveBeenCalledWith(
      'https://mizuki.example/internal/mizuki/functional-readiness',
      expect.objectContaining({
        method: 'GET',
        headers: expect.objectContaining({ authorization: `Bearer ${'p'.repeat(32)}` }),
        redirect: 'error',
      }),
    );
  });

  it('fails closed for a partial response or unlisted target', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn(async () =>
        Response.json({ status: 'ok', service: 'mizuki-api', checks: { database: 'ok' } }),
      ),
    );
    await expect(createProbe().probe('srv-production123')).rejects.toMatchObject({
      code: 'application_probe_unhealthy',
    });
    vi.stubGlobal(
      'fetch',
      vi.fn(async () => Response.json({ ok: true, extra: true })),
    );
    await expect(createProbe().probe('srv-shadow123')).rejects.toMatchObject({
      code: 'application_probe_unhealthy',
    });
    await expect(createProbe().probe('srv-other123')).rejects.toMatchObject({
      code: 'probe_service_denied',
    });
  });
});

function createProbe(): HttpApplicationGateway {
  return new HttpApplicationGateway({
    targets: new Map([
      ['srv-shadow123', { role: 'shadow', url: 'http://mizuki-shadow:10000/deployz' }],
      [
        'srv-production123',
        {
          role: 'production',
          url: 'https://mizuki.example/internal/mizuki/functional-readiness',
          token: 'p'.repeat(32),
        },
      ],
    ]),
    timeoutMs: 1_000,
  });
}
