import { createServer } from 'node:http';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { createApp, type AppDependencies } from './app.js';

const servers: ReturnType<typeof createServer>[] = [];

afterEach(async () => {
  await Promise.all(
    servers.splice(0).map(
      (server) =>
        new Promise<void>((resolve, reject) => {
          server.close((cause) => (cause ? reject(cause) : resolve()));
        }),
    ),
  );
});

describe('bounty retirement API', () => {
  it('requires admin authentication and answers with the offers it closed', async () => {
    const retireUnfundableOffers = vi.fn(async () => ['bounty-1', 'bounty-2']);
    const app = createApp({
      config: { adminToken: 'admin-secret' },
      store: {},
      bounties: { retireUnfundableOffers },
    } as unknown as AppDependencies);
    const server = createServer(app);
    servers.push(server);
    await new Promise<void>((resolve) => server.listen(0, '127.0.0.1', resolve));
    const address = server.address();
    if (!address || typeof address === 'string') throw new Error('test server did not bind');
    const url = `http://127.0.0.1:${address.port}/v1/admin/bounties/retire`;

    const unauthorized = await fetch(url, { method: 'POST' });
    expect(unauthorized.status).toBe(401);
    expect(retireUnfundableOffers).not.toHaveBeenCalled();

    const response = await fetch(url, {
      method: 'POST',
      headers: { authorization: 'Bearer admin-secret' },
    });

    expect(response.status).toBe(200);
    expect(await response.json()).toEqual({ retired: ['bounty-1', 'bounty-2'] });
    expect(retireUnfundableOffers).toHaveBeenCalledTimes(1);
  });
});
