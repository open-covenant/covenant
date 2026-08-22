import { createHash } from 'node:crypto';
import { DomainRuleError, assertNonEmpty } from './state-machine.js';

export type JsonValue =
  | null
  | boolean
  | number
  | string
  | readonly JsonValue[]
  | { readonly [key: string]: JsonValue };

export type IdempotencyRecord<Result> = {
  key: string;
  fingerprint: string;
  result: Result;
  recordedAt: string;
};

export type IdempotencyDecision<Result> =
  | { kind: 'new'; key: string; fingerprint: string }
  | { kind: 'replay'; record: IdempotencyRecord<Result> }
  | { kind: 'conflict'; record: IdempotencyRecord<Result>; fingerprint: string };

function canonicalJson(value: JsonValue): string {
  if (value === null || typeof value === 'boolean' || typeof value === 'string') {
    return JSON.stringify(value);
  }
  if (typeof value === 'number') {
    if (!Number.isFinite(value)) {
      throw new DomainRuleError('INVALID_PAYLOAD', 'Idempotency payload numbers must be finite');
    }
    return JSON.stringify(value);
  }
  if (Array.isArray(value)) {
    return `[${value.map(canonicalJson).join(',')}]`;
  }

  const entries = Object.entries(value).sort(([left], [right]) => left.localeCompare(right));
  return `{${entries
    .map(([key, entry]) => `${JSON.stringify(key)}:${canonicalJson(entry)}`)
    .join(',')}}`;
}

export function normalizeIdempotencyKey(value: string): string {
  const key = assertNonEmpty(value, 'idempotency key');
  if (key.length > 128 || !/^[A-Za-z0-9][A-Za-z0-9._:/-]*$/.test(key)) {
    throw new DomainRuleError(
      'INVALID_IDEMPOTENCY_KEY',
      'Idempotency keys must be 1-128 safe ASCII characters',
    );
  }
  return key;
}

export function fingerprintPayload(payload: JsonValue): string {
  return createHash('sha256').update(canonicalJson(payload)).digest('hex');
}

export function decideIdempotency<Result>(
  records: readonly IdempotencyRecord<Result>[],
  keyValue: string,
  payload: JsonValue,
): IdempotencyDecision<Result> {
  const key = normalizeIdempotencyKey(keyValue);
  const fingerprint = fingerprintPayload(payload);
  const existing = records.find((record) => record.key === key);

  if (!existing) {
    return { kind: 'new', key, fingerprint };
  }
  if (existing.fingerprint === fingerprint) {
    return { kind: 'replay', record: existing };
  }
  return { kind: 'conflict', record: existing, fingerprint };
}

export function requireNewIdempotency<Result>(
  decision: IdempotencyDecision<Result>,
): Extract<IdempotencyDecision<Result>, { kind: 'new' }> {
  if (decision.kind === 'replay') {
    throw new DomainRuleError('IDEMPOTENCY_REPLAY', 'Operation has already completed');
  }
  if (decision.kind === 'conflict') {
    throw new DomainRuleError(
      'IDEMPOTENCY_CONFLICT',
      'Idempotency key was already used with a different payload',
    );
  }
  return decision;
}
