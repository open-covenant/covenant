import { describe, expect, it } from 'vitest';
import { GET } from './route';

describe('health endpoint', () => {
  it('does not depend on the commercial API', async () => {
    const response = GET();

    expect(response.status).toBe(200);
    expect(response.headers.get('cache-control')).toBe('no-store');
    await expect(response.json()).resolves.toEqual({ ok: true, buildId: 'development' });
  });
});
