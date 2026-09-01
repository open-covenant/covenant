import { createServer, type IncomingMessage, type Server, type ServerResponse } from 'node:http';
import { timingSafeEqual } from 'node:crypto';
import type { FacilitatorConfig } from './config.js';

export interface FacilitatorApi {
  supported(): unknown;
  verify(paymentPayload: unknown, paymentRequirements: unknown): Promise<unknown>;
  settle(paymentPayload: unknown, paymentRequirements: unknown): Promise<unknown>;
  /**
   * Reports whether this facilitator can actually settle right now. A static
   * 200 would keep the runtime routing payments here after the fee payer ran
   * dry or the RPC went away, and every payment would fail at settlement
   * instead of being refused up front.
   */
  readiness?(): Promise<{ feePayerLamports: number }>;
}

/**
 * Enough for a few hundred settlements at the ~10k lamports each one has cost
 * in practice, and comfortably above the rent-exempt minimum. Below this the
 * service reports unready so the operator hears about it before payers do.
 */
const MIN_FEE_PAYER_LAMPORTS = 5_000_000;

class RequestError extends Error {
  constructor(
    readonly status: number,
    message: string,
  ) {
    super(message);
  }
}

/**
 * The fee payer signs and broadcasts whatever passes verification, so an
 * unauthenticated caller could spend its balance on their own payments even
 * though they cannot redirect ours. The runtime is the only intended caller.
 */
function authorize(request: IncomingMessage, token: string): void {
  const header = request.headers.authorization;
  const presented = typeof header === 'string' ? header.replace(/^Bearer\s+/i, '') : '';
  const expected = Buffer.from(token, 'utf8');
  const actual = Buffer.from(presented, 'utf8');
  if (actual.length !== expected.length || !timingSafeEqual(actual, expected)) {
    throw new RequestError(401, 'unauthorized');
  }
}

async function readJson(request: IncomingMessage, limit: number): Promise<unknown> {
  const chunks: Buffer[] = [];
  let received = 0;
  for await (const chunk of request) {
    received += chunk.length;
    if (received > limit) throw new RequestError(413, 'request body is too large');
    chunks.push(chunk as Buffer);
  }
  if (received === 0) throw new RequestError(400, 'request body is required');
  try {
    return JSON.parse(Buffer.concat(chunks).toString('utf8'));
  } catch {
    throw new RequestError(400, 'request body must be JSON');
  }
}

function writeJson(response: ServerResponse, status: number, body: unknown): void {
  const payload = JSON.stringify(body);
  response.writeHead(status, {
    'content-type': 'application/json',
    'cache-control': 'no-store',
    'content-length': Buffer.byteLength(payload),
  });
  response.end(payload);
}

export function createFacilitatorServer(
  api: FacilitatorApi,
  config: Pick<FacilitatorConfig, 'token' | 'maxRequestBytes'>,
  log: (event: Record<string, unknown>) => void = (event) => console.log(JSON.stringify(event)),
): Server {
  return createServer((request, response) => {
    void handle(request, response, api, config, log).catch((error) => {
      const status = error instanceof RequestError ? error.status : 500;
      const message = error instanceof RequestError ? error.message : 'facilitator error';
      if (status >= 500) log({ event: 'facilitator_error', message: String(error) });
      if (!response.headersSent) writeJson(response, status, { error: message });
    });
  });
}

async function handle(
  request: IncomingMessage,
  response: ServerResponse,
  api: FacilitatorApi,
  config: Pick<FacilitatorConfig, 'token' | 'maxRequestBytes'>,
  log: (event: Record<string, unknown>) => void,
): Promise<void> {
  const path = new URL(request.url ?? '/', 'http://facilitator.invalid').pathname;

  if (request.method === 'GET' && path === '/healthz') {
    writeJson(response, 200, { ok: true });
    return;
  }

  if (request.method === 'GET' && path === '/readyz') {
    if (!api.readiness) {
      writeJson(response, 200, { ok: true });
      return;
    }
    try {
      const { feePayerLamports } = await api.readiness();
      const ok = feePayerLamports >= MIN_FEE_PAYER_LAMPORTS;
      if (!ok) log({ event: 'facilitator_fee_payer_low', feePayerLamports });
      writeJson(response, ok ? 200 : 503, { ok, feePayerLamports });
    } catch (error) {
      log({ event: 'facilitator_not_ready', message: String(error) });
      writeJson(response, 503, { ok: false });
    }
    return;
  }

  if (request.method === 'GET' && path === '/supported') {
    authorize(request, config.token);
    writeJson(response, 200, api.supported());
    return;
  }

  if (request.method === 'POST' && (path === '/verify' || path === '/settle')) {
    authorize(request, config.token);
    const body = (await readJson(request, config.maxRequestBytes)) as {
      paymentPayload?: unknown;
      paymentRequirements?: unknown;
    };
    if (!body || typeof body !== 'object' || !body.paymentPayload || !body.paymentRequirements) {
      throw new RequestError(400, 'paymentPayload and paymentRequirements are required');
    }
    if (path === '/verify') {
      const result = (await api.verify(body.paymentPayload, body.paymentRequirements)) as {
        isValid?: boolean;
        invalidReason?: string;
      };
      log({ event: 'facilitator_verify', isValid: result?.isValid, reason: result?.invalidReason });
      writeJson(response, 200, result);
      return;
    }
    const result = (await api.settle(body.paymentPayload, body.paymentRequirements)) as {
      success?: boolean;
      transaction?: string;
      errorReason?: string;
    };
    log({
      event: 'facilitator_settle',
      success: result?.success,
      transaction: result?.transaction,
      reason: result?.errorReason,
    });
    writeJson(response, 200, result);
    return;
  }

  throw new RequestError(404, 'not found');
}
