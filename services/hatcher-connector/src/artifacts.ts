// Public download links for files a GPU job produced. The coding gateway keeps
// them on its private disk; this route forwards GET /artifacts/:token/:name to it.
// The 192-bit token is the authorization, so only well-formed paths are forwarded
// and nothing but the file and its hash comes back.

import { Readable } from 'node:stream';
import type { FastifyInstance } from 'fastify';

const TOKEN = /^[A-Za-z0-9_-]{32}$/;
const NAME = /^[A-Za-z0-9][A-Za-z0-9._-]{0,127}$/;
const SHA256 = /^[a-f0-9]{64}$/;
const MAX_BYTES = 64 * 1024 * 1024;
const TIMEOUT_MS = 30_000;

export function registerArtifactProxy(
  app: FastifyInstance,
  origin: string,
  fetcher: typeof fetch = fetch,
): void {
  app.get<{ Params: { token: string; name: string } }>('/artifacts/:token/:name', async (req, reply) => {
    const { token, name } = req.params;
    if (!TOKEN.test(token) || !NAME.test(name)) return reply.code(404).send({ error: 'not found' });
    let upstream: Response;
    try {
      upstream = await fetcher(`${origin}/artifacts/${token}/${encodeURIComponent(name)}`, {
        signal: AbortSignal.timeout(TIMEOUT_MS),
      });
    } catch {
      return reply.code(502).send({ error: 'artifact store unavailable' });
    }
    if (upstream.status === 404) return reply.code(404).send({ error: 'not found' });
    const length = Number(upstream.headers.get('content-length'));
    const sha256 = upstream.headers.get('x-content-sha256') ?? '';
    if (
      upstream.status !== 200 ||
      !upstream.body ||
      !Number.isSafeInteger(length) ||
      length < 0 ||
      length > MAX_BYTES ||
      !SHA256.test(sha256)
    ) {
      await upstream.body?.cancel().catch(() => undefined);
      return reply.code(502).send({ error: 'artifact store unavailable' });
    }
    return reply
      .code(200)
      .headers({
        'content-type': 'application/octet-stream',
        'content-length': String(length),
        'content-disposition': `attachment; filename="${name}"`,
        'x-content-sha256': sha256,
        'cache-control': 'private, no-store',
        'x-content-type-options': 'nosniff',
      })
      .send(Readable.fromWeb(upstream.body as import('node:stream/web').ReadableStream));
  });
}

export function artifactOrigin(raw: string | undefined): string | undefined {
  if (!raw || !raw.trim()) return undefined;
  let url: URL;
  try {
    url = new URL(raw.trim());
  } catch {
    throw new Error('HATCHER_ARTIFACT_ORIGIN must be a URL such as http://covenant-hatcher-gateway:10000');
  }
  if (!['http:', 'https:'].includes(url.protocol) || url.username || url.password || url.search || url.hash)
    throw new Error('HATCHER_ARTIFACT_ORIGIN must be a plain http(s) origin');
  return `${url.origin}${url.pathname}`.replace(/\/+$/, '');
}
