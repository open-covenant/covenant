import { createHash } from 'node:crypto';
import Fastify from 'fastify';
import { describe, expect, it, vi } from 'vitest';
import { artifactOrigin, registerArtifactProxy } from './artifacts.js';

const TOKEN = 'Ab3_-'.repeat(6) + 'xy';
const bytes = Buffer.from('embeddings');
const sha = createHash('sha256').update(bytes).digest('hex');

function app(fetcher: typeof fetch) {
  const server = Fastify();
  registerArtifactProxy(server, 'http://gateway.internal:10000', fetcher);
  return server;
}

describe('artifact proxy', () => {
  it('streams the stored file with its hash from the private gateway', async () => {
    const fetcher = vi.fn(async () =>
      new Response(bytes, { headers: { 'content-length': String(bytes.length), 'x-content-sha256': sha } }),
    );
    const res = await app(fetcher as unknown as typeof fetch).inject(`/artifacts/${TOKEN}/embeddings.npy`);
    expect(res.statusCode).toBe(200);
    expect(res.rawPayload.equals(bytes)).toBe(true);
    expect(res.headers).toMatchObject({
      'x-content-sha256': sha,
      'content-type': 'application/octet-stream',
      'content-disposition': 'attachment; filename="embeddings.npy"',
      'cache-control': 'private, no-store',
    });
    expect(fetcher).toHaveBeenCalledWith(
      `http://gateway.internal:10000/artifacts/${TOKEN}/embeddings.npy`,
      expect.anything(),
    );
  });

  it.each([`/artifacts/short/embeddings.npy`, `/artifacts/${TOKEN}/..%2Fsecret`, `/artifacts/${TOKEN}/.env`])(
    'does not forward %s',
    async (path) => {
      const fetcher = vi.fn();
      const res = await app(fetcher as unknown as typeof fetch).inject(path);
      expect(res.statusCode).toBe(404);
      expect(fetcher).not.toHaveBeenCalled();
    },
  );

  it('passes through a missing artifact and hides upstream failures', async () => {
    const missing = vi.fn(async () => new Response('{}', { status: 404 }));
    expect((await app(missing as unknown as typeof fetch).inject(`/artifacts/${TOKEN}/a.bin`)).statusCode).toBe(404);
    const unsigned = vi.fn(async () => new Response(bytes, { headers: { 'content-length': String(bytes.length) } }));
    expect((await app(unsigned as unknown as typeof fetch).inject(`/artifacts/${TOKEN}/a.bin`)).statusCode).toBe(502);
    const down = vi.fn(async () => {
      throw new Error('ECONNREFUSED 10.0.0.1');
    });
    const res = await app(down as unknown as typeof fetch).inject(`/artifacts/${TOKEN}/a.bin`);
    expect(res.statusCode).toBe(502);
    expect(res.body).not.toContain('10.0.0.1');
  });

  it('accepts only a plain origin', () => {
    expect(artifactOrigin('http://covenant-hatcher-gateway:10000/')).toBe('http://covenant-hatcher-gateway:10000');
    expect(artifactOrigin(undefined)).toBeUndefined();
    for (const bad of ['ftp://x', 'http://u:p@x', 'http://x?y=1', 'nope'])
      expect(() => artifactOrigin(bad)).toThrow();
  });
});
