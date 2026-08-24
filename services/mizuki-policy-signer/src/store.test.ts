import { describe, expect, it } from 'vitest';
import { InMemoryOperationStore } from './store.js';

describe('operation recovery order', () => {
  it('rotates attempted operations behind untouched work', async () => {
    const store = new InMemoryOperationStore();
    const start = new Date('2026-08-01T00:00:00.000Z');

    for (let index = 0; index < 21; index += 1) {
      await store.reserve(
        {
          id: `operation-${index}`,
          idempotencyKey: `idempotency-${index}`,
          resourceKey: `resource-${index}`,
          requestHash: String(index).padStart(64, '0'),
          kind: 'escrow_refund',
          amountUsdCents: 1,
          spendBucket: 'none',
          asset: 'SOL',
          recipient: 'recipient',
          details: {},
        },
        100,
        new Date(start.getTime() + index),
      );
    }

    const firstBatch = await store.listRecoverable(20);
    expect(firstBatch.map(({ id }) => id)).toEqual(
      Array.from({ length: 20 }, (_, index) => `operation-${index}`),
    );

    const leased = await store.acquireLease('operation-0', 'worker', new Date(), 5_000);
    expect(leased).not.toBeNull();
    await store.update('operation-0', 'worker', leased!.version, {
      status: 'reconciling',
      errorCode: 'operator_evidence_required',
    });
    await store.releaseLease('operation-0', 'worker');

    const secondBatch = await store.listRecoverable(20);
    expect(secondBatch.map(({ id }) => id)).toEqual(
      Array.from({ length: 20 }, (_, index) => `operation-${index + 1}`),
    );
  });
});
