import { describe, expect, it } from 'vitest';
import {
  DomainRuleError,
  assertExpectedRevision,
  assertTransition,
  canTransition,
  timestampMs,
  type TransitionTable,
} from './state-machine.js';
import {
  decideIdempotency,
  fingerprintPayload,
  normalizeIdempotencyKey,
  requireNewIdempotency,
  type IdempotencyRecord,
} from './idempotency.js';

describe('state machine guards', () => {
  const table: TransitionTable<'pending' | 'done'> = {
    pending: ['done'],
    done: [],
  };

  it('recognizes allowed transitions and rejects invalid ones with a stable code', () => {
    expect(canTransition(table, 'pending', 'done')).toBe(true);
    expect(canTransition(table, 'done', 'pending')).toBe(false);
    expect(() => assertTransition(table, 'done', 'pending', 'Task')).toThrowError(
      expect.objectContaining<Partial<DomainRuleError>>({ code: 'INVALID_TRANSITION' }),
    );
  });

  it('rejects stale and malformed revisions', () => {
    expect(() => assertExpectedRevision(2, 1)).toThrowError(
      expect.objectContaining<Partial<DomainRuleError>>({ code: 'STALE_REVISION' }),
    );
    expect(() => assertExpectedRevision(0, -1)).toThrowError(
      expect.objectContaining<Partial<DomainRuleError>>({ code: 'INVALID_REVISION' }),
    );
  });

  it('rejects invalid timestamps', () => {
    expect(() => timestampMs('not-a-date')).toThrowError(
      expect.objectContaining<Partial<DomainRuleError>>({ code: 'INVALID_TIMESTAMP' }),
    );
  });
});

describe('idempotency', () => {
  const at = '2026-08-22T10:00:00.000Z';

  it('fingerprints objects independently of key insertion order', () => {
    expect(fingerprintPayload({ amount: 200, payer: 'alice' })).toBe(
      fingerprintPayload({ payer: 'alice', amount: 200 }),
    );
    expect(fingerprintPayload(['alice', 200])).not.toBe(fingerprintPayload([200, 'alice']));
  });

  it('distinguishes new, replayed, and conflicting operations', () => {
    const payload = { amount: 200, payer: 'alice' } as const;
    const initial = decideIdempotency([], 'refund:job-1', payload);
    expect(initial).toMatchObject({ kind: 'new', key: 'refund:job-1' });
    if (initial.kind !== 'new') throw new Error('unreachable');

    const record: IdempotencyRecord<{ transaction: string }> = {
      key: initial.key,
      fingerprint: initial.fingerprint,
      result: { transaction: 'tx-1' },
      recordedAt: at,
    };
    expect(decideIdempotency([record], initial.key, payload)).toEqual({
      kind: 'replay',
      record,
    });
    expect(decideIdempotency([record], initial.key, { ...payload, amount: 201 })).toMatchObject({
      kind: 'conflict',
      record,
    });
  });

  it('rejects unsafe keys and non-finite payloads', () => {
    expect(() => normalizeIdempotencyKey('contains spaces')).toThrowError(
      expect.objectContaining<Partial<DomainRuleError>>({ code: 'INVALID_IDEMPOTENCY_KEY' }),
    );
    expect(() => fingerprintPayload(Number.NaN)).toThrowError(
      expect.objectContaining<Partial<DomainRuleError>>({ code: 'INVALID_PAYLOAD' }),
    );
  });

  it('requires a new decision when duplicate execution is unsafe', () => {
    const record: IdempotencyRecord<string> = {
      key: 'release:1',
      fingerprint: fingerprintPayload({ escrow: '1' }),
      result: 'tx-1',
      recordedAt: at,
    };
    const replay = decideIdempotency([record], record.key, { escrow: '1' });
    expect(() => requireNewIdempotency(replay)).toThrowError(
      expect.objectContaining<Partial<DomainRuleError>>({ code: 'IDEMPOTENCY_REPLAY' }),
    );
  });
});
