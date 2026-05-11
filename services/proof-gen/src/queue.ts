import { Queue, type ConnectionOptions } from 'bullmq';
import IORedis from 'ioredis';
import type { PublicInputs } from './schema.js';

export const QUEUE_NAME = 'proof-gen';
const DLQ_NAME = 'proof-gen-dlq';

export function buildDlq(connection: ConnectionOptions): Queue<ProveJobData & { error: string }> {
  return new Queue<ProveJobData & { error: string }>(DLQ_NAME, {
    connection,
    defaultJobOptions: {
      attempts: 1,
      removeOnComplete: false,
      removeOnFail: false,
    },
  });
}

export type ProveJobData = {
  circuit_id: string;
  public_inputs: PublicInputs;
  witness_ciphertext: string;
  witness_iv: string;
  witness_tag: string;
  // Envelope-wrapped witness AES key. Unwrapped by the worker using the
  // PROOFGEN_WITNESS_WRAP_KEY env held outside Redis. See
  // witness-envelope.ts for the format and threat model.
  witness_key_wrapped: string;
  agent_did: string;
  public_inputs_hash: string;
};

export type ProveJobResult = {
  proof: import('snarkjs').Groth16Proof;
  proof_hex: string;
  public_signals: string[];
  public_input_words: string[];
};

export function redisConnection(url: string): IORedis {
  return new IORedis(url, {
    maxRetriesPerRequest: null,
    enableReadyCheck: true,
  });
}

export function buildQueue(connection: ConnectionOptions): Queue<ProveJobData, ProveJobResult> {
  return new Queue<ProveJobData, ProveJobResult>(QUEUE_NAME, {
    connection,
    defaultJobOptions: {
      attempts: 3,
      backoff: { type: 'exponential', delay: 5_000 },
      removeOnComplete: { age: 3600 },
      removeOnFail: { age: 86_400 },
    },
  });
}

export function resultKey(jobId: string): string {
  return `proof-gen:result:${jobId}`;
}

export function cacheKey(hash: string): string {
  return `proof-gen:cache:${hash}`;
}
