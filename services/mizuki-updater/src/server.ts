import { timingSafeEqual } from 'node:crypto';
import { createServer, type IncomingMessage, type Server, type ServerResponse } from 'node:http';
import { z, ZodError } from 'zod';
import { publicAudit, publicUpgrade, signedProposalSchema, UpdaterError } from './domain.js';
import type { UpdaterMetrics } from './metrics.js';
import type { UpgradeRepository } from './store.js';
import type { UpdaterService } from './updater.js';

const upgradeIdSchema = z.string().uuid();
const proposalIdSchema = z
  .string()
  .min(1)
  .max(128)
  .regex(/^[A-Za-z0-9][A-Za-z0-9._:-]*$/);
const idempotencyKeySchema = z
  .string()
  .min(8)
  .max(128)
  .regex(/^[A-Za-z0-9][A-Za-z0-9._:-]*$/);
const promotionControlSchema = z
  .object({
    promotionsEnabled: z.boolean(),
    expectedRevision: z.number().int().nonnegative(),
    reason: z.string().trim().min(8).max(500),
  })
  .strict();

export interface UpdaterServerDependencies {
  service: UpdaterService;
  repository: UpgradeRepository;
  metrics: UpdaterMetrics;
  authToken: string;
  readToken: string;
}

export function createUpdaterServer(deps: UpdaterServerDependencies): Server {
  return createServer(async (request, response) => {
    try {
      await route(request, response, deps);
    } catch (error) {
      writeError(response, error, deps.metrics);
    }
  });
}

async function route(
  request: IncomingMessage,
  response: ServerResponse,
  deps: UpdaterServerDependencies,
): Promise<void> {
  const method = request.method ?? 'GET';
  const url = new URL(request.url ?? '/', 'http://updater.local');

  if (method === 'GET' && url.pathname === '/health') {
    try {
      await deps.repository.health();
      writeJson(response, 200, { status: 'ok', service: 'mizuki-updater' });
    } catch {
      writeJson(response, 503, { status: 'unavailable', service: 'mizuki-updater' });
    }
    return;
  }

  if (method === 'POST' && url.pathname === '/v1/upgrades') {
    requireBearer(request, [deps.authToken]);
    requireJson(request);
    const idempotencyKey = idempotencyKeySchema.parse(request.headers['idempotency-key']);
    const proposal = signedProposalSchema.parse(await readJson(request));
    const upgrade = await deps.service.submit(proposal, idempotencyKey);
    deps.service.kick(upgrade.id);
    writeJson(response, 202, { upgrade: publicUpgrade(upgrade) });
    return;
  }

  if (method === 'PUT' && url.pathname === '/v1/admin/promotion-control') {
    requireBearer(request, [deps.authToken]);
    requireJson(request);
    const input = promotionControlSchema.parse(await readJson(request));
    const control = await deps.repository.updatePromotionControl(
      { ...input, updatedBy: 'write_authority' },
      new Date(),
    );
    writeJson(response, 200, { control });
    return;
  }

  requireBearer(request, [deps.authToken, deps.readToken]);

  if (method === 'GET' && url.pathname === '/v1/admin/promotion-control') {
    writeJson(response, 200, { control: await deps.repository.promotionControl() });
    return;
  }

  if (method === 'GET' && url.pathname === '/metrics') {
    const body = deps.metrics.render(await deps.repository.stats());
    response.writeHead(200, {
      'content-type': 'text/plain; version=0.0.4; charset=utf-8',
      'cache-control': 'no-store',
      'content-length': Buffer.byteLength(body),
    });
    response.end(body);
    return;
  }

  const auditMatch = /^\/v1\/upgrades\/([^/]+)\/audit$/.exec(url.pathname);
  if (method === 'GET' && auditMatch) {
    const id = upgradeIdSchema.parse(auditMatch[1]);
    if (!(await deps.service.get(id))) {
      throw new UpdaterError('upgrade_not_found', 'Upgrade was not found', 404);
    }
    const receipts = (await deps.service.audit(id)).map(publicAudit);
    writeJson(response, 200, { receipts });
    return;
  }

  const proposalMatch = /^\/v1\/proposals\/([^/]+)$/.exec(url.pathname);
  if (method === 'GET' && proposalMatch) {
    const proposalId = proposalIdSchema.parse(decodePathSegment(proposalMatch[1]));
    const upgrade = await deps.service.getByProposalId(proposalId);
    if (!upgrade) throw new UpdaterError('upgrade_not_found', 'Upgrade was not found', 404);
    const audit = await deps.service.audit(upgrade.id);
    writeJson(response, 200, {
      upgrade: publicUpgrade(upgrade),
      auditHeadHash: audit.at(-1)?.hash ?? null,
    });
    return;
  }

  const upgradeMatch = /^\/v1\/upgrades\/([^/]+)$/.exec(url.pathname);
  if (method === 'GET' && upgradeMatch) {
    const id = upgradeIdSchema.parse(upgradeMatch[1]);
    const upgrade = await deps.service.get(id);
    if (!upgrade) throw new UpdaterError('upgrade_not_found', 'Upgrade was not found', 404);
    writeJson(response, 200, { upgrade: publicUpgrade(upgrade) });
    return;
  }

  throw new UpdaterError('not_found', 'Route was not found', 404);
}

function decodePathSegment(value: string): string {
  try {
    return decodeURIComponent(value);
  } catch {
    throw new UpdaterError('invalid_request', 'Path contains invalid encoding', 400);
  }
}

async function readJson(request: IncomingMessage): Promise<unknown> {
  const chunks: Buffer[] = [];
  let size = 0;
  for await (const raw of request) {
    const chunk = Buffer.isBuffer(raw) ? raw : Buffer.from(raw);
    size += chunk.length;
    if (size > 128 * 1024) {
      throw new UpdaterError('body_too_large', 'Request body exceeds 128 KiB', 413);
    }
    chunks.push(chunk);
  }
  try {
    return JSON.parse(Buffer.concat(chunks).toString('utf8'));
  } catch {
    throw new UpdaterError('invalid_json', 'Request body is not valid JSON', 400);
  }
}

function requireJson(request: IncomingMessage): void {
  const contentType = request.headers['content-type']?.split(';', 1)[0]?.trim();
  if (contentType !== 'application/json') {
    throw new UpdaterError('unsupported_media_type', 'Content-Type must be application/json', 415);
  }
}

function requireBearer(request: IncomingMessage, expected: readonly string[]): void {
  const authorization = request.headers.authorization;
  if (!authorization?.startsWith('Bearer ')) {
    throw new UpdaterError('unauthorized', 'Bearer authentication is required', 401);
  }
  const supplied = Buffer.from(authorization.slice(7));
  const valid = expected.some((value) => {
    const target = Buffer.from(value);
    return supplied.length === target.length && timingSafeEqual(supplied, target);
  });
  if (!valid) {
    throw new UpdaterError('unauthorized', 'Bearer authentication is invalid', 401);
  }
}

function writeJson(response: ServerResponse, status: number, payload: unknown): void {
  const body = JSON.stringify(payload);
  response.writeHead(status, {
    'content-type': 'application/json; charset=utf-8',
    'cache-control': 'no-store',
    'content-length': Buffer.byteLength(body),
  });
  response.end(body);
}

function writeError(response: ServerResponse, error: unknown, metrics: UpdaterMetrics): void {
  if (response.headersSent) {
    response.destroy();
    return;
  }
  if (error instanceof ZodError) {
    metrics.increment('rejections');
    writeJson(response, 400, {
      error: {
        code: 'invalid_request',
        message: 'Request failed schema validation',
        issues: error.issues.map((issue) => ({
          path: issue.path.join('.'),
          message: issue.message,
        })),
      },
    });
    return;
  }
  if (error instanceof UpdaterError) {
    metrics.increment(error.status < 500 ? 'rejections' : 'errors');
    if (error.status === 401) response.setHeader('www-authenticate', 'Bearer');
    writeJson(response, error.status, { error: { code: error.code, message: error.message } });
    return;
  }
  metrics.increment('errors');
  writeJson(response, 500, { error: { code: 'internal_error', message: 'Internal error' } });
}
