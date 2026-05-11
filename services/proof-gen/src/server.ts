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
  keyKey,
  resultKey,
  cacheKey,
  type ProveJobData,
} from './queue.js';
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
const KEY_TTL = Number(process.env.PROOFGEN_KEY_TTL_SEC ?? 600);

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

const BURST_LIMIT = 10;
const WINDOW_MS = 60_000;
const rateBuckets = new Map<string, { count: number; resetAt: number }>();

setInterval(() => {
  const now = Date.now();
  for (const [key, bucket] of rateBuckets) {
    if (now >= bucket.resetAt) rateBuckets.delete(key);
  }
}, WINDOW_MS);

function checkRateLimit(agentDid: string): { ok: true } | { ok: false; retry_after: number } {
  const now = Date.now();
  let bucket = rateBuckets.get(agentDid);
  if (!bucket || now >= bucket.resetAt) {
    bucket = { count: 0, resetAt: now + WINDOW_MS };
    rateBuckets.set(agentDid, bucket);
  }
  bucket.count++;
  if (bucket.count > BURST_LIMIT) {
    return { ok: false, retry_after: Math.ceil((bucket.resetAt - now) / 1000) };
  }
  return { ok: true };
}

function encryptWitness(priv: PrivateInputs): {
  ciphertext: string;
  iv: string;
  tag: string;
  key: Buffer;
} {
  // AES-256-GCM with ephemeral per-job key. Key stored in Redis with short TTL
  // (see keyKey()) and deleted by the worker after decrypt.
  const key = randomBytes(32);
  const iv = randomBytes(12);
  const cipher = createCipheriv('aes-256-gcm', key, iv);
  const plaintext = Buffer.from(JSON.stringify(priv), 'utf8');
  const ct = Buffer.concat([cipher.update(plaintext), cipher.final()]);
  plaintext.fill(0);
  return {
    ciphertext: ct.toString('base64'),
    iv: iv.toString('base64'),
    tag: cipher.getAuthTag().toString('base64'),
    key,
  };
}

function hashPublicInputs(circuit_id: string, pub: PublicInputs): string {
  const canonical = JSON.stringify([
    circuit_id,
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

    const rl = checkRateLimit(agent.agent_did);
    if (!rl.ok) return reply.code(429).send({ error: 'rate_limited', retry_after: rl.retry_after });

    const parsed = ProveRequestSchema.safeParse(req.body);
    if (!parsed.success) {
      return reply.code(400).send({ error: 'invalid_body', issues: parsed.error.issues });
    }
    const { circuit_id, public_inputs, private_inputs } = parsed.data;

    const pubHash = hashPublicInputs(circuit_id, public_inputs);

    const cached = await connection.get(cacheKey(pubHash));
    if (cached) {
      cacheHits.inc({ circuit: circuit_id });
      return reply.code(200).send({ status: 'completed', cached: true, ...JSON.parse(cached) });
    }

    const enc = encryptWitness(private_inputs);
    const jobId = randomUUID();
    await connection.set(keyKey(jobId), enc.key.toString('base64'), 'EX', KEY_TTL);
    enc.key.fill(0);

    const data: ProveJobData = {
      circuit_id,
      public_inputs,
      witness_ciphertext: enc.ciphertext,
      witness_iv: enc.iv,
      witness_tag: enc.tag,
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
  process.on('SIGTERM', close);
  process.on('SIGINT', close);

  return { app, queue, connection };
}

async function main() {
  const { app } = await buildServer();
  await app.listen({ port: PORT, host: '0.0.0.0' });
  logger.info({ port: PORT, artifacts_dir: ARTIFACTS_DIR }, 'proof-gen api up');
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
