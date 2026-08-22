import { getApiBaseUrl } from '@/lib/api';

export const dynamic = 'force-dynamic';
export const runtime = 'nodejs';

const forwardedRequestHeaders = [
  'accept',
  'authorization',
  'content-type',
  'cookie',
  'idempotency-key',
  'last-event-id',
  'payment-signature',
];

const forwardedResponseHeaders = [
  'cache-control',
  'content-type',
  'location',
  'payment-required',
  'payment-response',
  'set-cookie',
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
  const target = `${getApiBaseUrl()}/${pathname}${source.search}`;
  const headers = new Headers();
  for (const name of forwardedRequestHeaders) {
    const value = request.headers.get(name);
    if (value) headers.set(name, value);
  }
  headers.set('x-mizuki-web-proxy', '1');

  try {
    const body =
      request.method === 'GET' || request.method === 'HEAD'
        ? undefined
        : await request.arrayBuffer();
    const upstream = await fetch(target, {
      method: request.method,
      headers,
      body,
      redirect: 'manual',
      cache: 'no-store',
      signal: request.signal,
    });
    const responseHeaders = new Headers();
    for (const name of forwardedResponseHeaders) {
      const value = upstream.headers.get(name);
      if (value) responseHeaders.set(name, value);
    }
    responseHeaders.set('x-content-type-options', 'nosniff');
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

export const GET = proxy;
export const POST = proxy;
export const PUT = proxy;
export const PATCH = proxy;
export const DELETE = proxy;
