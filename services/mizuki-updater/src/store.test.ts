import { describe, expect, it } from 'vitest';
import { newUpgrade } from './domain.js';
import { InMemoryUpgradeRepository } from './store.js';
import { proposalFixture } from './test-utils.js';

const NOW = new Date('2026-08-22T12:00:00.000Z');

describe('upgrade repository', () => {
  it('starts with promotions closed and rejects stale control revisions', async () => {
    const repository = new InMemoryUpgradeRepository();
    const record = await repository.reserve(
      newUpgrade(proposalFixture(NOW).proposal, 'promotion-control-1'),
      NOW,
    );
    await expect(repository.reservePromotion(record.id, NOW)).resolves.toMatchObject({
      reserved: false,
      reason: 'disabled',
      control: { promotionsEnabled: false, revision: 0 },
    });

    const enabled = await repository.updatePromotionControl(
      {
        promotionsEnabled: true,
        expectedRevision: 0,
        reason: 'approved controlled promotion',
        updatedBy: 'write_authority',
      },
      NOW,
    );
    expect(enabled).toMatchObject({ promotionsEnabled: true, revision: 1 });
    await expect(
      repository.updatePromotionControl(
        {
          promotionsEnabled: false,
          expectedRevision: 0,
          reason: 'stale pause request',
          updatedBy: 'write_authority',
        },
        NOW,
      ),
    ).rejects.toMatchObject({ code: 'promotion_control_conflict' });
    await expect(repository.promotionControlAudit()).resolves.toHaveLength(2);
  });

  it('holds one promotion reservation until its owner releases it', async () => {
    const repository = new InMemoryUpgradeRepository();
    const first = await repository.reserve(
      newUpgrade(proposalFixture(NOW).proposal, 'promotion-owner-1'),
      NOW,
    );
    const secondFixture = proposalFixture(NOW);
    secondFixture.proposal.manifest.proposalId = 'upgrade-2';
    secondFixture.attestBenchmark(secondFixture.proposal.manifest);
    secondFixture.attestReview(secondFixture.proposal.manifest);
    const secondProposal = secondFixture.signManifest(secondFixture.proposal.manifest);
    const second = await repository.reserve(newUpgrade(secondProposal, 'promotion-owner-2'), NOW);
    await repository.updatePromotionControl(
      {
        promotionsEnabled: true,
        expectedRevision: 0,
        reason: 'approved controlled promotion',
        updatedBy: 'write_authority',
      },
      NOW,
    );
    await expect(repository.reservePromotion(first.id, NOW)).resolves.toMatchObject({
      reserved: true,
      control: { activeUpgradeId: first.id },
    });
    await expect(repository.reservePromotion(second.id, NOW)).resolves.toMatchObject({
      reserved: false,
      reason: 'busy',
      control: { activeUpgradeId: first.id },
    });
    await expect(
      repository.releasePromotion(first.id, new Date(NOW.getTime() + 1_000)),
    ).rejects.toMatchObject({ code: 'promotion_release_not_terminal' });
    expect(await repository.acquireLease(first.id, 'worker-a', NOW, 10_000)).toBe(true);
    await repository.transition(
      first.id,
      first.version,
      'worker-a',
      { state: 'failed' },
      { event: 'test_failure' },
      new Date(NOW.getTime() + 1_000),
    );
    await repository.releasePromotion(first.id, new Date(NOW.getTime() + 1_000));
    await expect(
      repository.reservePromotion(second.id, new Date(NOW.getTime() + 1_000)),
    ).resolves.toMatchObject({
      reserved: true,
      control: { activeUpgradeId: second.id },
    });
    await expect(repository.promotionControlAudit()).resolves.toMatchObject([
      { revision: 0, activeUpgradeId: null },
      { revision: 1, activeUpgradeId: null },
      { revision: 2, activeUpgradeId: first.id },
      { revision: 3, activeUpgradeId: null },
      { revision: 4, activeUpgradeId: second.id },
    ]);
  });

  it('reserves proposal and idempotency keys exactly once', async () => {
    const repository = new InMemoryUpgradeRepository();
    const fixture = proposalFixture(NOW);
    const input = newUpgrade(fixture.proposal, 'idempotency-1');
    const first = await repository.reserve(input, NOW);
    const replay = await repository.reserve({ ...input, id: crypto.randomUUID() }, NOW);
    expect(replay.id).toBe(first.id);
    expect(await repository.getByProposalId(input.proposalId)).toMatchObject({ id: first.id });
    expect(await repository.getByProposalId('missing-proposal')).toBeNull();

    const changed = structuredClone(fixture.proposal);
    changed.signature = `${'A'.repeat(86)}==`;
    await expect(
      repository.reserve(newUpgrade(changed, 'idempotency-1'), NOW),
    ).rejects.toMatchObject({ code: 'idempotency_conflict' });
  });

  it('uses a lease and optimistic version for transitions', async () => {
    const repository = new InMemoryUpgradeRepository();
    const input = newUpgrade(proposalFixture(NOW).proposal, 'idempotency-1');
    const record = await repository.reserve(input, NOW);
    expect(await repository.acquireLease(record.id, 'worker-a', NOW, 10_000)).toBe(true);
    expect(await repository.acquireLease(record.id, 'worker-b', NOW, 10_000)).toBe(false);

    const next = await repository.transition(
      record.id,
      record.version,
      'worker-a',
      { state: 'verifying_artifact' },
      { event: 'verification_started' },
      NOW,
    );
    expect(next.version).toBe(1);
    await expect(
      repository.transition(
        record.id,
        record.version,
        'worker-a',
        { state: 'verifying_artifact' },
        { event: 'stale' },
        NOW,
      ),
    ).rejects.toMatchObject({ code: 'version_conflict' });
  });

  it('rejects invalid state transitions and chains audit hashes', async () => {
    const repository = new InMemoryUpgradeRepository();
    const record = await repository.reserve(
      newUpgrade(proposalFixture(NOW).proposal, 'idempotency-1'),
      NOW,
    );
    await repository.acquireLease(record.id, 'worker', NOW, 10_000);
    await expect(
      repository.transition(
        record.id,
        record.version,
        'worker',
        { state: 'completed' },
        { event: 'invalid' },
        NOW,
      ),
    ).rejects.toMatchObject({ code: 'invalid_state_transition' });

    await repository.transition(
      record.id,
      record.version,
      'worker',
      { state: 'verifying_artifact' },
      { event: 'verification_started' },
      NOW,
    );
    const receipts = await repository.audit(record.id);
    expect(receipts).toHaveLength(2);
    expect(receipts[1].previousHash).toBe(receipts[0].hash);
    expect(receipts[0].hash).toMatch(/^[a-f0-9]{64}$/);
  });

  it('lists only due, non-leased work', async () => {
    const repository = new InMemoryUpgradeRepository();
    const record = await repository.reserve(
      newUpgrade(proposalFixture(NOW).proposal, 'idempotency-1'),
      NOW,
    );
    expect(await repository.listRunnable(NOW, 10)).toEqual([record.id]);
    await repository.acquireLease(record.id, 'worker', NOW, 10_000);
    expect(await repository.listRunnable(NOW, 10)).toEqual([]);
    expect(await repository.listRunnable(new Date(NOW.getTime() + 10_001), 10)).toEqual([
      record.id,
    ]);
  });
});
