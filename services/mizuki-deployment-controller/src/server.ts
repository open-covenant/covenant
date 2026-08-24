import { timingSafeEqual } from 'node:crypto';
import { createServer, type IncomingMessage, type Server, type ServerResponse } from 'node:http';
import { z, ZodError } from 'zod';
import type { DeploymentController } from './controller.js';
import {
  ControllerError,
  externalId,
  finalizeRequestSchema,
  promotionRequestSchema,
  rollbackRequestSchema,
  shadowRequestSchema,
} from './domain.js';
import type { OperationStore } from './store.js';

const idempotencyKey = z
  .string()
  .min(3)
  .max(128)
  .regex(/^[A-Za-z0-9][A-Za-z0-9._:-]*$/);

export interface ControllerServerDependencies {
  controller: DeploymentController;
  store: OperationStore;
  authToken: string;
}

export function createControllerServer(deps: ControllerServerDependencies): Server {
  return createServer(async (request, response) => {
    try {
      await route(request, response, deps);
    } catch (error) {
      writeError(response, error);
    }
  });
}

async function route(
  request: IncomingMessage,
  response: ServerResponse,
  deps: ControllerServerDependencies,
): Promise<void> {
  requireBearer(request, deps.authToken);
  const method = request.method ?? 'GET';
  const url = new URL(request.url ?? '/', 'http://deployment-controller.local');

  if (method === 'GET' && url.pathname === '/healthz') {
    await deps.store.health();
    writeJson(response, 200, { status: 'ok', service: 'mizuki-deployment-controller' });
    return;
  }
  if (method === 'GET' && url.pathname === '/readyz') {
    await deps.controller.readiness();
    writeJson(response, 200, { status: 'ok', service: 'mizuki-deployment-controller' });
    return;
  }
  if (method === 'POST' && url.pathname === '/v1/deployments/shadow') {
    requireJson(request);
    const key = idempotencyKey.parse(request.headers['idempotency-key']);
    const input = shadowRequestSchema.parse(await readJson(request));
    writeJson(response, 200, await deps.controller.startShadow(input, key));
    return;
  }
  if (method === 'POST' && url.pathname === '/v1/deployments/promote') {
    requireJson(request);
    const key = idempotencyKey.parse(request.headers['idempotency-key']);
    const input = promotionRequestSchema.parse(await readJson(request));
    writeJson(response, 200, await deps.controller.promote(input, key));
    return;
  }
  if (method === 'POST' && url.pathname === '/v1/deployments/finalize') {
    requireJson(request);
    const key = idempotencyKey.parse(request.headers['idempotency-key']);
    const input = finalizeRequestSchema.parse(await readJson(request));
    writeJson(response, 200, await deps.controller.finalize(input, key));
    return;
  }
  if (method === 'POST' && url.pathname === '/v1/deployments/rollback') {
    requireJson(request);
    const key = idempotencyKey.parse(request.headers['idempotency-key']);
    const input = rollbackRequestSchema.parse(await readJson(request));
    writeJson(response, 200, await deps.controller.rollback(input, key));
    return;
  }

  const shadow = /^\/v1\/deployments\/shadow\/([^/]+)\/health$/.exec(url.pathname);
  if (method === 'GET' && shadow) {
    const deploymentId = externalId.parse(decodePath(shadow[1]));
    writeJson(response, 200, await deps.controller.shadowHealth(deploymentId));
    return;
  }
  const production = /^\/v1\/deployments\/production\/([^/]+)\/health$/.exec(url.pathname);
  if (method === 'GET' && production) {
    const deploymentId = externalId.parse(decodePath(production[1]));
    writeJson(response, 200, await deps.controller.promotionHealth(deploymentId));
    return;
  }
  throw new ControllerError('not_found', 'Route was not found', 404);
}

function requireBearer(request: IncomingMessage, expected: string): void {
  const header = request.headers.authorization;
  if (!header?.startsWith('Bearer ')) unauthorized();
  const supplied = Buffer.from(header!.slice(7));
  const target = Buffer.from(expected);
  if (supplied.length !== target.length || !timingSafeEqual(supplied, target)) unauthorized();
}

function unauthorized(): never {
  throw new ControllerError('unauthorized', 'Bearer authentication is invalid', 401);
}

function requireJson(request: IncomingMessage): void {
  if (request.headers['content-type']?.split(';', 1)[0]?.trim() !== 'application/json') {
    throw new ControllerError(
      'unsupported_media_type',
      'Content-Type must be application/json',
      415,
    );
  }
}

async function readJson(request: IncomingMessage): Promise<unknown> {
  const chunks: Buffer[] = [];
  let size = 0;
  for await (const value of request) {
    const chunk = Buffer.isBuffer(value) ? value : Buffer.from(value);
    size += chunk.length;
    if (size > 64 * 1024) {
      throw new ControllerError('body_too_large', 'Request body exceeds 64 KiB', 413);
    }
    chunks.push(chunk);
  }
  try {
    return JSON.parse(Buffer.concat(chunks).toString('utf8'));
  } catch {
    throw new ControllerError('invalid_json', 'Request body is not valid JSON', 400);
  }
}

function decodePath(value: string): string {
  try {
    return decodeURIComponent(value);
  } catch {
    throw new ControllerError('invalid_path', 'Path contains invalid encoding', 400);
  }
}

function writeJson(response: ServerResponse, status: number, payload: unknown): void {
  const body = JSON.stringify(payload);
  response.writeHead(status, {
    'content-type': 'application/json; charset=utf-8',
    'cache-control': 'no-store',
    'content-length': Buffer.byteLength(body),
    'x-content-type-options': 'nosniff',
  });
  response.end(body);
}

function writeError(response: ServerResponse, error: unknown): void {
  if (response.headersSent) {
    response.destroy();
    return;
  }
  if (error instanceof ZodError) {
    writeJson(response, 400, {
      message: 'Request failed schema validation',
      error: {
        code: 'invalid_request',
        issues: error.issues.map((issue) => ({
          path: issue.path.join('.'),
          message: issue.message,
        })),
      },
    });
    return;
  }
  if (error instanceof ControllerError) {
    if (error.status === 401) response.setHeader('www-authenticate', 'Bearer');
    if (error.retryable) {
      response.setHeader('retry-after', String(error.retryAfterSeconds ?? 5));
    }
    writeJson(response, error.status, {
      message: error.message,
      error: { code: error.code },
    });
    return;
  }
  writeJson(response, 500, {
    message: 'Internal error',
    error: { code: 'internal_error' },
  });
}
