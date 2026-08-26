export const dynamic = 'force-dynamic';
export const runtime = 'nodejs';

const MAX_BODY_BYTES = 8_192;
const ALLOWED_METHODS = new Set(['getAccountInfo', 'getLatestBlockhash']);

type RpcRequest = {
  jsonrpc: '2.0';
  id: string | number | null;
  method: string;
  params?: unknown[];
};

export async function POST(request: Request): Promise<Response> {
  if (declaredBodyBytes(request.headers.get('content-length')) > MAX_BODY_BYTES) {
    return Response.json({ error: 'RPC request is too large' }, { status: 413 });
  }

  const body = await boundedBody(request, MAX_BODY_BYTES);
  if (!body) return Response.json({ error: 'Invalid RPC request' }, { status: 400 });

  const rpcRequest = parseRpcRequest(body);
  if (!rpcRequest || !ALLOWED_METHODS.has(rpcRequest.method)) {
    return Response.json({ error: 'RPC method is not allowed' }, { status: 400 });
  }

  const upstream = rpcEndpoint();
  if (!upstream) {
    return Response.json({ error: 'Solana RPC is unavailable' }, { status: 503 });
  }

  try {
    const response = await fetch(upstream, {
      method: 'POST',
      headers: { accept: 'application/json', 'content-type': 'application/json' },
      body: JSON.stringify(rpcRequest),
      cache: 'no-store',
      signal: AbortSignal.timeout(10_000),
    });
    const payload = await response.arrayBuffer();
    return new Response(payload, {
      status: response.status,
      headers: {
        'cache-control': 'private, no-store',
        'content-type': response.headers.get('content-type') ?? 'application/json',
        'x-content-type-options': 'nosniff',
      },
    });
  } catch {
    return Response.json({ error: 'Solana RPC is unavailable' }, { status: 502 });
  }
}

function parseRpcRequest(body: Uint8Array): RpcRequest | undefined {
  let value: unknown;
  try {
    value = JSON.parse(new TextDecoder().decode(body));
  } catch {
    return undefined;
  }
  if (!isRecord(value) || value.jsonrpc !== '2.0' || typeof value.method !== 'string') {
    return undefined;
  }
  if (
    value.id !== null &&
    typeof value.id !== 'string' &&
    (typeof value.id !== 'number' || !Number.isSafeInteger(value.id))
  ) {
    return undefined;
  }
  if (value.params !== undefined && !Array.isArray(value.params)) return undefined;
  return {
    jsonrpc: '2.0',
    id: value.id,
    method: value.method,
    ...(value.params ? { params: value.params } : {}),
  };
}

function rpcEndpoint(): string | undefined {
  const configured = process.env.MIZUKI_SOLANA_RPC_URL;
  if (!configured) return undefined;
  try {
    const url = new URL(configured);
    if (
      url.protocol !== 'https:' ||
      url.hostname !== 'mainnet.helius-rpc.com' ||
      url.username ||
      url.password ||
      !url.searchParams.get('api-key')
    ) {
      return undefined;
    }
    return url.toString();
  } catch {
    return undefined;
  }
}

function declaredBodyBytes(value: string | null): number {
  const candidate = value?.trim();
  if (!candidate || !/^[0-9]+$/.test(candidate)) return 0;
  const length = BigInt(candidate);
  return length > BigInt(Number.MAX_SAFE_INTEGER) ? Number.POSITIVE_INFINITY : Number(length);
}

async function boundedBody(request: Request, maxBytes: number): Promise<Uint8Array | undefined> {
  if (!request.body) return undefined;
  const reader = request.body.getReader();
  const chunks: Uint8Array[] = [];
  let length = 0;
  while (true) {
    const { done, value } = await reader.read();
    if (done) break;
    length += value.byteLength;
    if (length > maxBytes) {
      await reader.cancel().catch(() => undefined);
      return undefined;
    }
    chunks.push(value);
  }
  const body = new Uint8Array(length);
  let offset = 0;
  for (const chunk of chunks) {
    body.set(chunk, offset);
    offset += chunk.byteLength;
  }
  return body;
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null && !Array.isArray(value);
}
