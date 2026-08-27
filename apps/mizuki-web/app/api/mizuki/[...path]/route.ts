import { isIP } from 'node:net';

export const dynamic = 'force-dynamic';
export const runtime = 'nodejs';

const MAX_BODY_BYTES = 64_000;
const MAX_WEBHOOK_BODY_BYTES = 1_000_000;

const forwardedRequestHeaders = [
  'accept',
  'authorization',
  'content-type',
  'cookie',
  'idempotency-key',
  'last-event-id',
  'origin',
  'payment-signature',
  'x-mizuki-csrf-token',
];

const githubWebhookHeaders = ['x-github-delivery', 'x-github-event', 'x-hub-signature-256'];

const forwardedResponseHeaders = [
  'cache-control',
  'clear-site-data',
  'content-type',
  'location',
  'payment-required',
  'payment-response',
  'x-request-id',
];

async function proxy(
  request: Request,
  context: { params: Promise<{ path: string[] }> },
): Promise<Response> {
  const { path } = await context.params;
  if (!path.length || path[0] !== 'v1' || path.includes('admin')) {
    return Response.json({ error: 'Public API path not allowed' }, { status: 404 });
  }

  const source = new URL(request.url);
  const pathname = path.map((part) => encodeURIComponent(part)).join('/');
  const webhook = path.length === 3 && path.join('/') === 'v1/github/webhook';
  const maxBodyBytes = webhook ? MAX_WEBHOOK_BODY_BYTES : MAX_BODY_BYTES;
  const apiBaseUrl = (process.env.MIZUKI_API_URL || 'http://127.0.0.1:8787').replace(/\/$/, '');
  const target = `${apiBaseUrl}/${pathname}${source.search}`;
  const proxySecret = process.env.MIZUKI_WEB_PROXY_SECRET;
  if (!proxySecret || Buffer.byteLength(proxySecret, 'utf8') < 32) {
    return Response.json({ error: 'Mizuki API proxy is unavailable' }, { status: 503 });
  }

  const forwardsBody = request.method !== 'GET' && request.method !== 'HEAD';
  if (forwardsBody && declaredBodyBytes(request.headers.get('content-length')) > maxBodyBytes) {
    return bodyTooLarge(maxBodyBytes);
  }

  const headers = new Headers();
  for (const name of forwardedRequestHeaders) {
    const value = request.headers.get(name);
    if (value) headers.set(name, value);
  }
  if (webhook) {
    for (const name of githubWebhookHeaders) {
      const value = request.headers.get(name);
      if (value) headers.set(name, value);
    }
  }
  headers.set('x-mizuki-proxy-secret', proxySecret);
  headers.set('x-mizuki-forwarded-proto', publicScheme(source));
  const clientIp = renderClientIp(request.headers.get('cf-connecting-ip'));
  if (clientIp) headers.set('x-mizuki-client-ip', clientIp);

  try {
    const buffered = forwardsBody ? await boundedBody(request, maxBodyBytes) : { body: undefined };
    if ('tooLarge' in buffered) return bodyTooLarge(maxBodyBytes);
    const upstream = await fetch(target, {
      method: request.method,
      headers,
      body: buffered.body,
      redirect: 'manual',
      cache: 'no-store',
      signal: request.signal,
    });
    const responseHeaders = new Headers();
    for (const name of forwardedResponseHeaders) {
      const value = upstream.headers.get(name);
      if (value) responseHeaders.set(name, value);
    }
    if (path[1] === 'account' || path[1] === 'jobs' || path[1] === 'auth') {
      responseHeaders.set('cache-control', 'private, no-store, max-age=0');
    }
    if (upstream.ok && path.join('/') === 'v1/auth/logout') {
      responseHeaders.set('cache-control', 'private, no-store');
      responseHeaders.set('clear-site-data', '"cache", "cookies", "storage"');
    }
    if (path.join('/') === 'v1/auth/github') {
      const location = responseHeaders.get('location');
      if (location) responseHeaders.set('location', githubAccountPickerUrl(location));
    }
    const setCookies = upstream.headers.getSetCookie();
    for (const value of setCookies) responseHeaders.append('set-cookie', value);
    responseHeaders.set('x-content-type-options', 'nosniff');
    responseHeaders.set(
      'x-mizuki-web-build',
      process.env.NEXT_PUBLIC_MIZUKI_BUILD_ID?.trim() || 'development',
    );
    return new Response(upstream.body, {
      status: upstream.status,
      statusText: upstream.statusText,
      headers: responseHeaders,
    });
  } catch (cause) {
    return Response.json(
      {
        error:
          cause instanceof Error && cause.name === 'AbortError'
            ? 'Request was cancelled'
            : 'Mizuki API is unavailable',
      },
      { status: 502 },
    );
  }
}

function githubAccountPickerUrl(location: string): string {
  try {
    const target = new URL(location);
    if (
      target.protocol !== 'https:' ||
      target.hostname !== 'github.com' ||
      target.pathname !== '/login/oauth/authorize'
    ) {
      return location;
    }
    target.searchParams.set('prompt', 'select_account');
    return target.toString();
  } catch {
    return location;
  }
}

export const GET = proxy;
export const POST = proxy;
export const PUT = proxy;
export const PATCH = proxy;
export const DELETE = proxy;

function declaredBodyBytes(value: string | null): number {
  const candidate = value?.trim();
  if (!candidate || !/^[0-9]+$/.test(candidate)) return 0;
  const length = BigInt(candidate);
  return length > BigInt(Number.MAX_SAFE_INTEGER) ? Number.POSITIVE_INFINITY : Number(length);
}

async function boundedBody(
  request: Request,
  maxBytes: number,
): Promise<{ body: ArrayBuffer | undefined } | { tooLarge: true }> {
  if (!request.body) return { body: undefined };

  const reader = request.body.getReader();
  const chunks: Uint8Array[] = [];
  let length = 0;
  while (true) {
    const { done, value } = await reader.read();
    if (done) break;
    length += value.byteLength;
    if (length > maxBytes) {
      await reader.cancel().catch(() => undefined);
      return { tooLarge: true };
    }
    chunks.push(value);
  }

  const body = new Uint8Array(length);
  let offset = 0;
  for (const chunk of chunks) {
    body.set(chunk, offset);
    offset += chunk.byteLength;
  }
  return { body: body.buffer };
}

function bodyTooLarge(maxBytes: number): Response {
  return Response.json({ error: `Request body exceeds ${maxBytes} bytes` }, { status: 413 });
}

function renderClientIp(value: string | null): string | undefined {
  if (!value) return undefined;
  const candidate = value.trim();
  return isIP(candidate) ? candidate.toLowerCase() : undefined;
}

function publicScheme(source: URL): 'http' | 'https' {
  const configured = process.env.NEXT_PUBLIC_MIZUKI_APP_URL;
  if (configured) {
    try {
      if (new URL(configured).protocol === 'https:') return 'https';
    } catch {
      // An invalid configured origin must not upgrade cookies.
    }
  }
  return source.protocol === 'https:' ? 'https' : 'http';
}
