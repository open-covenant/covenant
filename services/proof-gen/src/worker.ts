import { createDecipheriv } from 'node:crypto';
import { existsSync } from 'node:fs';
import { resolve } from 'node:path';
import http from 'node:http';
import { Worker } from 'bullmq';
import pino from 'pino';
import * as snarkjs from 'snarkjs';
import {
  QUEUE_NAME,
  redisConnection,
  resultKey,
  cacheKey,
  buildDlq,
  type ProveJobData,
  type ProveJobResult,
} from './queue.js';
import { ensureWitnessEnvelopeReady, unwrapWitnessKey } from './witness-envelope.js';
import { jobsTotal, proveDuration, registry } from './metrics.js';
import { encodeGroth16ProofHex, encodePublicInputWords } from './proof-format.js';

const logger = pino({
  level: process.env.LOG_LEVEL ?? 'info',
  redact: {
    paths: ['witness_ciphertext', 'witness_key', 'private_inputs'],
    censor: '[redacted]',
  },
});

const REDIS_URL = process.env.REDIS_URL ?? 'redis://127.0.0.1:6379';
const DEFAULT_ARTIFACTS_DIR = resolve(import.meta.dirname, '..', 'artifacts', 'task_completion', 'build');
const ARTIFACTS_DIR = resolve(process.env.CIRCUIT_ARTIFACTS_DIR ?? DEFAULT_ARTIFACTS_DIR);
const CONCURRENCY = Number(process.env.PROOFGEN_WORKER_CONCURRENCY ?? 1);
const RESULT_TTL = Number(process.env.PROOFGEN_RESULT_TTL_SEC ?? 3600);
const WORKER_HEALTH_PORT = Number(process.env.PROOFGEN_WORKER_HEALTH_PORT ?? 8786);

type CircuitArtifacts = { wasm: string; zkey: string };
const artifactCache = new Map<string, CircuitArtifacts>();

const CIRCUIT_ARTIFACTS: Record<string, CircuitArtifacts> = {
  'task_completion.v1': {
    wasm: resolve(ARTIFACTS_DIR, 'task_completion_js', 'task_completion.wasm'),
    zkey: resolve(ARTIFACTS_DIR, 'task_completion.zkey'),
  },
};

function loadArtifacts(circuit_id: string): CircuitArtifacts {
  const cached = artifactCache.get(circuit_id);
  if (cached) return cached;
  const paths = CIRCUIT_ARTIFACTS[circuit_id];
  if (!paths) throw new Error(`unknown circuit: ${circuit_id}`);
  if (!existsSync(paths.wasm)) throw new Error(`wasm missing: ${paths.wasm}`);
  if (!existsSync(paths.zkey)) throw new Error(`zkey missing: ${paths.zkey}`);
  artifactCache.set(circuit_id, paths);
  return paths;
}

type CircuitScalar = string | number | bigint;
type CircuitInput = Record<string, CircuitScalar | CircuitScalar[]>;

function isCircuitScalar(value: unknown): value is CircuitScalar {
  return typeof value === 'string' || typeof value === 'number' || typeof value === 'bigint';
}

function parseCircuitInput(value: unknown): CircuitInput {
  if (value === null || typeof value !== 'object' || Array.isArray(value)) {
    throw new Error('invalid witness payload');
  }

  const obj = value as { [key: string]: unknown };
  const out: CircuitInput = {};

  for (const key of Object.keys(obj)) {
    const raw = obj[key];
    if (isCircuitScalar(raw)) {
      out[key] = raw;
      continue;
    }

    if (Array.isArray(raw) && raw.every(isCircuitScalar)) {
      out[key] = raw;
      continue;
    }

    throw new Error(`invalid witness value for key: ${key}`);
  }

  return out;
}

function decryptWitness(data: ProveJobData, key: Buffer): CircuitInput {
  const iv = Buffer.from(data.witness_iv, 'base64');
  const tag = Buffer.from(data.witness_tag, 'base64');
  const ct = Buffer.from(data.witness_ciphertext, 'base64');
  const decipher = createDecipheriv('aes-256-gcm', key, iv);
  decipher.setAuthTag(tag);
  const plaintext = Buffer.concat([decipher.update(ct), decipher.final()]);
  try {
    const parsed = JSON.parse(plaintext.toString('utf8')) as unknown;
    return parseCircuitInput(parsed);
  } finally {
    plaintext.fill(0);
  }
}

export type WorkerHandles = {
  worker: Worker<ProveJobData, ProveJobResult>;
  healthServer: http.Server;
  close: () => Promise<void>;
};

export function startWorker(): WorkerHandles {
  const connection = redisConnection(REDIS_URL);
  const dlq = buildDlq(connection);

  // Health/metrics surface so k8s (or the operator's monitor of choice)
  // can distinguish "process running" from "BullMQ consumer wedged". The
  // /healthz body reports worker.isRunning() and the last-processed-at
  // timestamp; a stalled queue stays visible without scraping bullmq.
  let lastProcessedAt: number | null = null;
  const updateLastProcessedAt = () => {
    lastProcessedAt = Date.now();
  };

  const worker = new Worker<ProveJobData, ProveJobResult>(
    QUEUE_NAME,
    async (job) => {
      const { circuit_id, public_inputs, agent_did, public_inputs_hash } = job.data;
      const log = logger.child({ job_id: job.id, agent_did, circuit_id });
      const stopTimer = proveDuration.startTimer({ circuit: circuit_id });
      log.info('prove:start');

      const artifacts = loadArtifacts(circuit_id);

      const key = unwrapWitnessKey(job.data.witness_key_wrapped);

      let witness: CircuitInput;
      try {
        const priv = decryptWitness(job.data, key);
        witness = { ...public_inputs, ...priv };
      } finally {
        key.fill(0);
      }

      const { proof, publicSignals } = await snarkjs.groth16.fullProve(
        witness,
        artifacts.wasm,
        artifacts.zkey,
      );

      const result: ProveJobResult = {
        proof,
        proof_hex: encodeGroth16ProofHex(proof),
        public_signals: publicSignals,
        public_input_words: encodePublicInputWords(publicSignals),
      };

      const payload = JSON.stringify({ status: 'completed', ...result });
      await connection.set(resultKey(job.id!), payload, 'EX', RESULT_TTL);
      await connection.set(cacheKey(public_inputs_hash), payload, 'EX', RESULT_TTL);
      stopTimer();
      jobsTotal.inc({ circuit: circuit_id, status: 'completed' });
      updateLastProcessedAt();
      log.info({ public_inputs_hash }, 'prove:done');
      return result;
    },
    { connection, concurrency: CONCURRENCY },
  );

  worker.on('failed', async (job, err) => {
    logger.warn({ job_id: job?.id, err: err.message, attempts: job?.attemptsMade }, 'prove:failed');
    updateLastProcessedAt();
    if (job && job.attemptsMade >= (job.opts.attempts ?? 1)) {
      jobsTotal.inc({ circuit: job.data.circuit_id, status: 'dlq' });
      try {
        await dlq.add(
          'dead',
          { ...job.data, error: err.message },
          { jobId: `${job.id}-${Date.now()}` },
        );
      } catch (e) {
        const message = e instanceof Error ? e.message : String(e);
        logger.error({ err: message }, 'dlq:enqueue_failed');
      }
    } else {
      jobsTotal.inc({ circuit: job?.data.circuit_id ?? 'unknown', status: 'retry' });
    }
  });

  const healthServer = http.createServer((req, res) => {
    if (req.method !== 'GET') {
      res.writeHead(405).end();
      return;
    }
    if (req.url === '/healthz') {
      const body = JSON.stringify({
        ok: worker.isRunning(),
        running: worker.isRunning(),
        last_processed_at: lastProcessedAt,
        last_processed_age_ms: lastProcessedAt ? Date.now() - lastProcessedAt : null,
        concurrency: CONCURRENCY,
        artifacts_dir: ARTIFACTS_DIR,
      });
      res.writeHead(worker.isRunning() ? 200 : 503, { 'content-type': 'application/json' });
      res.end(body);
      return;
    }
    if (req.url === '/metrics') {
      registry
        .metrics()
        .then((m) => {
          res.writeHead(200, { 'content-type': registry.contentType });
          res.end(m);
        })
        .catch((err) => {
          res.writeHead(500).end(err instanceof Error ? err.message : String(err));
        });
      return;
    }
    res.writeHead(404).end();
  });
  healthServer.listen(WORKER_HEALTH_PORT, '0.0.0.0', () => {
    logger.info({ port: WORKER_HEALTH_PORT }, 'proof-gen worker health surface up');
  });

  const close = async (): Promise<void> => {
    await new Promise<void>((resolve) => healthServer.close(() => resolve()));
    await worker.close();
    connection.disconnect();
  };

  return { worker, healthServer, close };
}

const isEntry = import.meta.url === `file://${process.argv[1]}`;
if (isEntry) {
  process.on('unhandledRejection', (reason) => {
    logger.error({ err: reason }, 'proof-gen worker: unhandled rejection');
    process.exit(1);
  });
  process.on('uncaughtException', (err) => {
    logger.error({ err }, 'proof-gen worker: uncaught exception');
    process.exit(1);
  });
  ensureWitnessEnvelopeReady();
  const handles = startWorker();
  // SIGTERM/SIGINT listeners registered once at the entrypoint, not inside
  // startWorker(). Tests that call startWorker() repeatedly no longer
  // accumulate handlers up to Node's MaxListenersExceededWarning.
  const shutdown = (signal: string) => {
    logger.info({ signal }, 'proof-gen worker: shutting down');
    handles
      .close()
      .then(() => process.exit(0))
      .catch((err) => {
        logger.error({ err }, 'proof-gen worker: shutdown failed');
        process.exit(1);
      });
  };
  process.on('SIGTERM', () => shutdown('SIGTERM'));
  process.on('SIGINT', () => shutdown('SIGINT'));
  logger.info({ artifacts_dir: ARTIFACTS_DIR, concurrency: CONCURRENCY }, 'proof-gen worker up');
}
