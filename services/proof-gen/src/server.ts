import { createHash, randomBytes, createCipheriv } from 'node:crypto';
import { existsSync } from 'node:fs';
import { resolve } from 'node:path';
import { randomUUID } from 'node:crypto';
import Fastify from 'fastify';
import { jwtVerify } from 'jose';
import pino from 'pino';
import {
  ProveRequestSchema,
  JobIdParamsSchema,
  type PrivateInputs,
  type PublicInputs,
} from './schema.js';
import {
  buildQueue,
  redisConnection,
  resultKey,
  cacheKey,
  type ProveJobData,
} from './queue.js';
import {
  ensureWitnessEnvelopeReady,
  generateWitnessKey,
  wrapWitnessKey,
} from './witness-envelope.js';
import { checkRateLimit, RateLimitBackendError } from './rate-limit.js';
import { registry, cacheHits } from './metrics.js';

const logger = pino({
  level: process.env.LOG_LEVEL ?? 'info',
  redact: {
    paths: [
      'req.headers.authorization',
      'private_inputs',
      'witness_ciphertext',
      'witness_key',
      'aes_key',
    ],
    censor: '[redacted]',
  },
});

const REDIS_URL = process.env.REDIS_URL ?? 'redis://127.0.0.1:6379';
const PORT = Number(process.env.PROOFGEN_PORT ?? 8787);
const DEFAULT_ARTIFACTS_DIR = resolve(import.meta.dirname, '..', 'artifacts', 'task_completion', 'build');
const ARTIFACTS_DIR = resolve(process.env.CIRCUIT_ARTIFACTS_DIR ?? DEFAULT_ARTIFACTS_DIR);
const RATE_LIMIT_BURST = Number(process.env.PROOFGEN_RATE_LIMIT_BURST ?? 10);
const RATE_LIMIT_WINDOW_MS = Number(process.env.PROOFGEN_RATE_LIMIT_WINDOW_MS ?? 60_000);

const WASM_PATH = resolve(ARTIFACTS_DIR, 'task_completion_js', 'task_completion.wasm');
const ZKEY_PATH = resolve(ARTIFACTS_DIR, 'task_completion.zkey');
const VK_PATH = resolve(ARTIFACTS_DIR, 'verification_key.json');

function artifactsReady(): boolean {
  return existsSync(WASM_PATH) && existsSync(ZKEY_PATH);
}

const SESSION_SECRET = process.env.SESSION_SECRET;
const SESSION_ISSUER = 'covenant.portal';

let sessionKey: Uint8Array | null = null;
function getSessionKey(): Uint8Array {
  if (sessionKey) return sessionKey;
  if (!SESSION_SECRET || SESSION_SECRET.length < 32) {
    throw new Error('SESSION_SECRET env var required (>=32 chars)');
  }
  sessionKey = new TextEncoder().encode(SESSION_SECRET);
  return sessionKey;
}

async function resolveAgent(authHeader: string | undefined): Promise<{ agent_did: string } | null> {
  if (!authHeader || !authHeader.startsWith('Bearer ')) return null;
  const token = authHeader.slice('Bearer '.length).trim();
  if (!token) return null;
  try {
    const secret = getSessionKey();
    const { payload } = await jwtVerify(token, secret, {
      issuer: SESSION_ISSUER,
      algorithms: ['HS256'],
      audience: 'covenant:proof-gen',
    });
    const address = payload.sub;
    if (typeof address !== 'string') return null;
    return { agent_did: address };
  } catch {
    return null;
  }
}

function encryptWitness(priv: PrivateInputs): {
  ciphertext: string;
  iv: string;
  tag: string;
  wrappedKey: string;
} {
  // AES-256-GCM with ephemeral per-job key. The key is wrapped under the
  // long-lived PROOFGEN_WITNESS_WRAP_KEY env secret (held outside Redis)
  // before being shipped as part of the job payload. Redis compromise
  // alone does not yield plaintext.
  const key = generateWitnessKey();
  const iv = randomBytes(12);
  const cipher = createCipheriv('aes-256-gcm', key, iv);
  const plaintext = Buffer.from(JSON.stringify(priv), 'utf8');
  const ct = Buffer.concat([cipher.update(plaintext), cipher.final()]);
  plaintext.fill(0);
  const wrappedKey = wrapWitnessKey(key);
  key.fill(0);
  return {
    ciphertext: ct.toString('base64'),
    iv: iv.toString('base64'),
    tag: cipher.getAuthTag().toString('base64'),
    wrappedKey,
  };
}

function hashPublicInputs(
  circuit_id: string,
  pub: PublicInputs,
  agent_did: string,
): string {
  // agent_did is part of the cache key so two agents with identical
  // public inputs cannot share a cache slot. Without it, agent A's
  // earlier /prove poisons agent B's later /prove with the same inputs
  // (proof correctness is unaffected — the public signals are
  // identical — but the policy boundary leaks: agent B gets a result
  // they did not pay for / are not authorised for under any future
  // per-agent billing or capability gate).
  const canonical = JSON.stringify([
    circuit_id,
    agent_did,
    pub.task_hash,
    pub.result_hash,
    pub.deadline,
    pub.submitted_at,
    pub.criteria_root,
  ]);
  return createHash('sha256').update(canonical).digest('hex');
}

export async function buildServer() {
  const app = Fastify({ loggerInstance: logger });
  const connection = redisConnection(REDIS_URL);
  const queue = buildQueue(connection);

  app.get('/healthz', async () => {
    let redisStatus: 'up' | 'down' = 'down';
    try {
      const pong = await connection.ping();
      redisStatus = pong === 'PONG' ? 'up' : 'down';
    } catch {
      redisStatus = 'down';
    }
    const [waiting, active, failed] = await Promise.all([
      queue.getWaitingCount(),
      queue.getActiveCount(),
      queue.getFailedCount(),
    ]);
    const hasArtifacts = artifactsReady();
    const hasVk = existsSync(VK_PATH);
    return {
      ok: redisStatus === 'up' && hasArtifacts,
      redis: redisStatus,
      artifacts: hasArtifacts ? 'loaded' : 'missing',
      verification_key: hasVk ? 'present' : 'missing',
      circuits: hasArtifacts ? ['task_completion.v1'] : [],
      queue: { waiting, active, failed },
    };
  });

  app.get('/metrics', async (_req, reply) => {
    reply.header('content-type', registry.contentType);
    return registry.metrics();
  });

  app.post('/prove', async (req, reply) => {
    if (!artifactsReady()) {
      return reply.code(503).send({ error: 'no_artifacts' });
    }

    const agent = await resolveAgent(req.headers.authorization);
    if (!agent) return reply.code(401).send({ error: 'unauthorized' });

    let rl;
    try {
      rl = await checkRateLimit(agent.agent_did, {
        connection,
        limit: RATE_LIMIT_BURST,
        windowMs: RATE_LIMIT_WINDOW_MS,
      });
    } catch (e) {
      if (e instanceof RateLimitBackendError) {
        logger.error({ err: e.message }, 'rate-limit backend error');
        return reply.code(503).send({ error: 'rate_limit_backend_unavailable' });
      }
      throw e;
    }
    if (!rl.ok) return reply.code(429).send({ error: 'rate_limited', retry_after: rl.retry_after });

    const parsed = ProveRequestSchema.safeParse(req.body);
    if (!parsed.success) {
      return reply.code(400).send({ error: 'invalid_body', issues: parsed.error.issues });
    }
    const { circuit_id, public_inputs, private_inputs } = parsed.data;

    const pubHash = hashPublicInputs(circuit_id, public_inputs, agent.agent_did);

    const cached = await connection.get(cacheKey(pubHash));
    if (cached) {
      cacheHits.inc({ circuit: circuit_id });
      return reply.code(200).send({ status: 'completed', cached: true, ...JSON.parse(cached) });
    }

    const enc = encryptWitness(private_inputs);
    const jobId = randomUUID();

    const data: ProveJobData = {
      circuit_id,
      public_inputs,
      witness_ciphertext: enc.ciphertext,
      witness_iv: enc.iv,
      witness_tag: enc.tag,
      witness_key_wrapped: enc.wrappedKey,
      agent_did: agent.agent_did,
      public_inputs_hash: pubHash,
    };
    await queue.add('prove', data, { jobId });

    return reply.code(202).send({ job_id: jobId, status: 'queued' });
  });

  app.get('/jobs/:id', async (req, reply) => {
    const params = JobIdParamsSchema.safeParse(req.params);
    if (!params.success) return reply.code(400).send({ error: 'invalid_id' });
    const { id } = params.data;

    const cached = await connection.get(resultKey(id));
    if (cached) {
      return reply.send(JSON.parse(cached));
    }

    const job = await queue.getJob(id);
    if (!job) return reply.code(404).send({ error: 'not_found' });

    const state = await job.getState();
    if (state === 'completed') {
      return reply.send({ status: 'completed', ...(job.returnvalue ?? {}) });
    }
    if (state === 'failed') {
      return reply.send({ status: 'failed', error: job.failedReason ?? 'unknown' });
    }
    return reply.send({ status: state });
  });

  const close = async () => {
    await app.close();
    await queue.close();
    connection.disconnect();
  };
  // SIGTERM/SIGINT listeners are NOT registered here. buildServer() may
  // be called repeatedly under test (each call would stack another
  // listener; Node warns at >10 with MaxListenersExceededWarning, and
  // none of them would deregister). The entrypoint below registers the
  // listeners once for the process lifetime.

  return { app, queue, connection, close };
}

async function main() {
  ensureWitnessEnvelopeReady();
  const handles = await buildServer();
  await handles.app.listen({ port: PORT, host: '0.0.0.0' });
  logger.info({ port: PORT, artifacts_dir: ARTIFACTS_DIR }, 'proof-gen api up');
  const shutdown = (signal: string) => {
    logger.info({ signal }, 'proof-gen api: shutting down');
    handles
      .close()
      .then(() => process.exit(0))
      .catch((err) => {
        logger.error({ err }, 'proof-gen api: shutdown failed');
        process.exit(1);
      });
  };
  process.on('SIGTERM', () => shutdown('SIGTERM'));
  process.on('SIGINT', () => shutdown('SIGINT'));
}

const isEntry = import.meta.url === `file://${process.argv[1]}`;
if (isEntry) {
  process.on('unhandledRejection', (reason) => {
    logger.error({ err: reason }, 'proof-gen api: unhandled rejection');
    process.exit(1);
  });
  process.on('uncaughtException', (err) => {
    logger.error({ err }, 'proof-gen api: uncaught exception');
    process.exit(1);
  });
  main().catch((err) => {
    logger.error({ err }, 'proof-gen api failed to start');
    process.exit(1);
  });
}
