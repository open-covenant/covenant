import { timingSafeEqual } from 'node:crypto';
import { createServer, type IncomingMessage, type Server, type ServerResponse } from 'node:http';
import { z, ZodError, type ZodType } from 'zod';
import {
  bindChallengeRequestSchema,
  bindEscrowRequestSchema,
  bindRefundLiabilityDeliveryRequestSchema,
  activatePaymentIntentRequestSchema,
  createPaymentIntentRequestSchema,
  createEscrowRequestSchema,
  dischargeRefundLiabilityRequestSchema,
  githubIdentityGrantRequestSchema,
  operationView,
  paymentIntentView,
  PolicyError,
  refundLiabilityView,
  refundEscrowRequestSchema,
  refundRequestSchema,
  reconcileRepositorySettlementRequestSchema,
  reconcilePaymentIntentRequestSchema,
  refundCommandView,
  registerRefundLiabilityRequestSchema,
  repositoryAdmissionRequestSchema,
  repositoryAdmissionView,
  repositoryReadinessRequestSchema,
  releaseEscrowRequestSchema,
  validateRepositoryAdmissionRequestSchema,
} from './domain.js';
import type { SignerMetrics } from './metrics.js';
import type { PolicyService } from './policy.js';
import type { OperationStore } from './store.js';

const idempotencyKeySchema = z
  .string()
  .min(8)
  .max(128)
  .regex(/^[A-Za-z0-9][A-Za-z0-9._:-]*$/);
const operationIdSchema = z.string().uuid();
const MAX_REQUEST_BODY_BYTES = 128 * 1024;

export interface HttpServerDependencies {
  service: PolicyService;
  store: OperationStore;
  metrics: SignerMetrics;
  authToken: string;
}

export function createSignerServer(deps: HttpServerDependencies): Server {
  const server = createServer(async (request, response) => {
    try {
      await route(request, response, deps);
    } catch (error) {
      writeError(response, error, deps.metrics);
    }
  });
  server.headersTimeout = 5_000;
  server.requestTimeout = 10_000;
  server.keepAliveTimeout = 5_000;
  server.maxRequestsPerSocket = 100;
  return server;
}

async function route(
  request: IncomingMessage,
  response: ServerResponse,
  deps: HttpServerDependencies,
): Promise<void> {
  const method = request.method ?? 'GET';
  const url = new URL(request.url ?? '/', 'http://signer.local');

  if (method === 'GET' && url.pathname === '/health') {
    const readiness = await deps.service.probeReadiness();
    writeJson(response, readiness.healthy ? 200 : 503, { ok: readiness.healthy });
    return;
  }
  if (method === 'GET' && url.pathname === '/metrics') {
    const metrics = deps.metrics.render(await deps.store.stats());
    response.writeHead(200, {
      'content-type': 'text/plain; version=0.0.4; charset=utf-8',
      'cache-control': 'no-store',
    });
    response.end(metrics);
    return;
  }

  authenticate(request, deps.authToken);

  if (method === 'GET' && url.pathname === '/v1/readiness/evidence') {
    const evidence = await deps.service.probeReadiness();
    writeJson(response, evidence.healthy ? 200 : 503, evidence);
    return;
  }

  if (method === 'GET' && url.pathname === '/v1/readiness') {
    const readiness = await deps.service.readiness();
    writeJson(response, readiness.healthy ? 200 : 503, readiness);
    return;
  }

  if (method === 'POST' && url.pathname === '/v1/readiness/repository') {
    deps.metrics.increment('requests');
    const { repository } = await parseBody(request, repositoryReadinessRequestSchema);
    writeJson(response, 200, await deps.service.repositoryReadiness(repository));
    return;
  }

  if (method === 'POST' && url.pathname === '/v1/repository-admissions') {
    deps.metrics.increment('requests');
    const admission = await deps.service.createRepositoryAdmission(
      await parseBody(request, repositoryAdmissionRequestSchema),
      idempotencyKey(request),
    );
    writeJson(response, 201, repositoryAdmissionView(admission));
    return;
  }

  if (method === 'POST' && url.pathname === '/v1/payment-intents') {
    deps.metrics.increment('requests');
    const intent = await deps.service.createPaymentIntent(
      await parseBody(request, createPaymentIntentRequestSchema),
      idempotencyKey(request),
    );
    writeJson(response, 201, paymentIntentView(intent));
    return;
  }
  const paymentIntent = url.pathname.match(/^\/v1\/payment-intents\/([0-9a-f-]+)$/i);
  if (method === 'GET' && paymentIntent) {
    writeJson(
      response,
      200,
      paymentIntentView(
        await deps.service.getPaymentIntent(operationIdSchema.parse(paymentIntent[1])),
      ),
    );
    return;
  }
  const activatePaymentIntent = url.pathname.match(
    /^\/v1\/payment-intents\/([0-9a-f-]+)\/activate$/i,
  );
  if (method === 'POST' && activatePaymentIntent) {
    deps.metrics.increment('requests');
    const activation = await deps.service.activatePaymentIntent(
      operationIdSchema.parse(activatePaymentIntent[1]),
      await parseBody(request, activatePaymentIntentRequestSchema),
      idempotencyKey(request),
    );
    writeJson(response, 200, activation);
    return;
  }
  const reconcilePaymentIntent = url.pathname.match(
    /^\/v1\/payment-intents\/([0-9a-f-]+)\/reconcile$/i,
  );
  if (method === 'POST' && reconcilePaymentIntent) {
    deps.metrics.increment('requests');
    await parseBody(request, reconcilePaymentIntentRequestSchema);
    const reconciled = await deps.service.reconcilePaymentIntent(
      operationIdSchema.parse(reconcilePaymentIntent[1]),
      idempotencyKey(request),
    );
    writeJson(
      response,
      200,
      'refundLiability' in reconciled ? reconciled : paymentIntentView(reconciled),
    );
    return;
  }
  const validateAdmission = url.pathname.match(
    /^\/v1\/repository-admissions\/([0-9a-f-]+)\/validate$/i,
  );
  if (method === 'POST' && validateAdmission) {
    deps.metrics.increment('requests');
    const admission = await deps.service.validateRepositoryAdmission(
      operationIdSchema.parse(validateAdmission[1]),
      await parseBody(request, validateRepositoryAdmissionRequestSchema),
    );
    writeJson(response, 200, repositoryAdmissionView(admission));
    return;
  }
  const reconcileAdmissionSettlement = url.pathname.match(
    /^\/v1\/repository-admissions\/([0-9a-f-]+)\/settlements\/reconcile$/i,
  );
  if (method === 'POST' && reconcileAdmissionSettlement) {
    deps.metrics.increment('requests');
    const settlement = await deps.service.reconcileRepositorySettlement(
      operationIdSchema.parse(reconcileAdmissionSettlement[1]),
      await parseBody(request, reconcileRepositorySettlementRequestSchema),
    );
    writeJson(response, 200, settlement);
    return;
  }

  if (method === 'POST' && url.pathname === '/v1/github/identity-grants') {
    deps.metrics.increment('requests');
    const grant = await deps.service.issueGitHubIdentityGrant(
      await parseBody(request, githubIdentityGrantRequestSchema),
    );
    writeJson(response, 201, grant);
    return;
  }

  if (method === 'POST' && url.pathname === '/v1/refunds') {
    deps.metrics.increment('requests');
    const record = await deps.service.refund(
      await parseBody(request, refundRequestSchema),
      idempotencyKey(request),
    );
    writeJson(response, responseStatus(record.status), operationView(record));
    return;
  }
  if (method === 'POST' && url.pathname === '/v1/refund-liabilities') {
    deps.metrics.increment('requests');
    const liability = await deps.service.registerRefundLiability(
      await parseBody(request, registerRefundLiabilityRequestSchema),
      idempotencyKey(request),
    );
    writeJson(response, 201, refundLiabilityView(liability));
    return;
  }
  const dischargeLiability = url.pathname.match(
    /^\/v1\/refund-liabilities\/([0-9a-f-]+)\/discharge$/i,
  );
  if (method === 'POST' && dischargeLiability) {
    deps.metrics.increment('requests');
    const liability = await deps.service.dischargeRefundLiability(
      operationIdSchema.parse(dischargeLiability[1]),
      await parseBody(request, dischargeRefundLiabilityRequestSchema),
      idempotencyKey(request),
    );
    writeJson(response, 200, refundLiabilityView(liability));
    return;
  }
  const bindLiabilityDelivery = url.pathname.match(
    /^\/v1\/refund-liabilities\/([0-9a-f-]+)\/delivery-bindings$/i,
  );
  if (method === 'POST' && bindLiabilityDelivery) {
    deps.metrics.increment('requests');
    const liability = await deps.service.bindRefundLiabilityDelivery(
      operationIdSchema.parse(bindLiabilityDelivery[1]),
      await parseBody(request, bindRefundLiabilityDeliveryRequestSchema),
      idempotencyKey(request),
    );
    writeJson(response, 200, refundLiabilityView(liability));
    return;
  }
  if (method === 'POST' && url.pathname === '/v1/escrows') {
    deps.metrics.increment('requests');
    const record = await deps.service.createEscrow(
      await parseBody(request, createEscrowRequestSchema),
      idempotencyKey(request),
    );
    writeJson(response, responseStatus(record.status), operationView(record));
    return;
  }

  const bindChallenge = url.pathname.match(/^\/v1\/escrows\/([0-9a-f-]+)\/bind-challenges$/i);
  if (method === 'POST' && bindChallenge) {
    deps.metrics.increment('requests');
    const challenge = await deps.service.issueBindChallenge(
      operationIdSchema.parse(bindChallenge[1]),
      await parseBody(request, bindChallengeRequestSchema),
    );
    writeJson(response, 201, challenge);
    return;
  }

  const bind = url.pathname.match(/^\/v1\/escrows\/([0-9a-f-]+)\/bind$/i);
  if (method === 'POST' && bind) {
    deps.metrics.increment('requests');
    const record = await deps.service.bindEscrow(
      operationIdSchema.parse(bind[1]),
      await parseBody(request, bindEscrowRequestSchema),
      idempotencyKey(request),
    );
    writeJson(response, responseStatus(record.status), operationView(record));
    return;
  }

  const release = url.pathname.match(/^\/v1\/escrows\/([0-9a-f-]+)\/release$/i);
  if (method === 'POST' && release) {
    deps.metrics.increment('requests');
    const record = await deps.service.releaseEscrow(
      operationIdSchema.parse(release[1]),
      await parseBody(request, releaseEscrowRequestSchema),
      idempotencyKey(request),
    );
    writeJson(response, responseStatus(record.status), operationView(record));
    return;
  }

  const refund = url.pathname.match(/^\/v1\/escrows\/([0-9a-f-]+)\/refund$/i);
  if (method === 'POST' && refund) {
    deps.metrics.increment('requests');
    const record = await deps.service.refundEscrow(
      operationIdSchema.parse(refund[1]),
      await parseBody(request, refundEscrowRequestSchema),
      idempotencyKey(request),
    );
    writeJson(response, responseStatus(record.status), operationView(record));
    return;
  }

  const operation = url.pathname.match(/^\/v1\/operations\/([0-9a-f-]+)$/i);
  if (method === 'GET' && operation) {
    const record = await deps.service.get(operationIdSchema.parse(operation[1]));
    writeJson(response, 200, operationView(record));
    return;
  }

  const refundCommand = url.pathname.match(/^\/v1\/refund-commands\/([0-9a-f-]+)$/i);
  if (method === 'GET' && refundCommand) {
    const command = await deps.store.getRefundCommand(operationIdSchema.parse(refundCommand[1]));
    if (!command)
      throw new PolicyError('refund_command_not_found', 'Refund command was not found', 404);
    writeJson(response, 200, refundCommandView(command));
    return;
  }

  throw new PolicyError('not_found', 'Route was not found', 404);
}

function authenticate(request: IncomingMessage, expectedToken: string): void {
  const value = request.headers.authorization;
  if (!value?.startsWith('Bearer ')) {
    throw new PolicyError('unauthorized', 'Bearer authentication is required', 401);
  }
  const token = Buffer.from(value.slice(7));
  const expected = Buffer.from(expectedToken);
  if (token.length !== expected.length || !timingSafeEqual(token, expected)) {
    throw new PolicyError('unauthorized', 'Bearer authentication failed', 401);
  }
}

function idempotencyKey(request: IncomingMessage): string {
  const value = request.headers['idempotency-key'];
  if (Array.isArray(value)) {
    throw new PolicyError('invalid_idempotency_key', 'Idempotency-Key must be singular', 400);
  }
  return idempotencyKeySchema.parse(value);
}

async function parseBody<T>(request: IncomingMessage, schema: ZodType<T>): Promise<T> {
  const contentType = request.headers['content-type']?.split(';')[0]?.trim();
  if (contentType !== 'application/json') {
    throw new PolicyError('unsupported_media_type', 'Content-Type must be application/json', 415);
  }
  const chunks: Buffer[] = [];
  let length = 0;
  for await (const chunk of request) {
    const bytes = Buffer.isBuffer(chunk) ? chunk : Buffer.from(chunk);
    length += bytes.length;
    if (length > MAX_REQUEST_BODY_BYTES) {
      throw new PolicyError('body_too_large', 'Request body is too large', 413);
    }
    chunks.push(bytes);
  }
  try {
    return schema.parse(JSON.parse(Buffer.concat(chunks).toString('utf8')));
  } catch (error) {
    if (error instanceof ZodError) throw error;
    throw new PolicyError('invalid_json', 'Request body is not valid JSON', 400);
  }
}

function responseStatus(status: string): number {
  return status === 'finalized' ? 200 : 202;
}

function writeError(response: ServerResponse, error: unknown, metrics: SignerMetrics): void {
  if (response.headersSent) {
    response.end();
    return;
  }
  metrics.increment('rejections');
  if (error instanceof PolicyError) {
    writeJson(response, error.statusCode, {
      error: { code: error.code, message: error.message, retryable: error.retryable },
    });
    return;
  }
  if (error instanceof ZodError) {
    writeJson(response, 400, {
      error: {
        code: 'invalid_request',
        message: 'Request did not match the required schema',
        retryable: false,
      },
    });
    return;
  }
  metrics.increment('errors');
  writeJson(response, 500, {
    error: { code: 'internal_error', message: 'Signer request failed', retryable: true },
  });
}

function writeJson(response: ServerResponse, status: number, body: unknown): void {
  response.writeHead(status, {
    'content-type': 'application/json; charset=utf-8',
    'cache-control': 'no-store',
    'x-content-type-options': 'nosniff',
  });
  response.end(JSON.stringify(body));
}
