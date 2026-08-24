import { describe, expect, it } from 'vitest';
import { capabilityHandoff } from './capability-handoff.js';
import type { Capability, FailureRecord, Upgrade } from './domain/index.js';

const capability: Capability = {
  id: '11111111-1111-4111-8111-111111111111',
  key: 'model.route-reliability',
  name: 'Model Route Reliability',
  state: 'proposed',
  createdAt: '2026-08-22T12:00:00.000Z',
  updatedAt: '2026-08-22T12:00:00.000Z',
  revision: 1,
};

const upgrade: Upgrade = {
  id: '22222222-2222-4222-8222-222222222222',
  capabilityId: capability.id,
  triggerReasons: ['standard_job_failure', 'paid_job_failure', 'repeated_failure'],
  state: 'proposed',
  evidence: {},
  createdAt: '2026-08-22T12:00:00.000Z',
  updatedAt: '2026-08-22T12:00:00.000Z',
  revision: 0,
};

const failures: FailureRecord[] = [
  {
    id: 'bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb',
    capabilityKey: capability.key,
    normalizedCode: 'usepod_route_timed_out',
    jobClass: 'micro',
    occurredAt: '2026-08-22T11:30:00.000Z',
  },
  {
    id: 'aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa',
    capabilityKey: capability.key,
    normalizedCode: 'usepod_route_timed_out',
    jobClass: 'standard',
    occurredAt: '2026-08-22T11:00:00.000Z',
  },
];

describe('capability handoff', () => {
  it('has a deterministic hash independent of storage ordering and mutable rollout state', () => {
    const first = capabilityHandoff({ capability, upgrade, failures });
    const progressed = capabilityHandoff({
      capability: {
        ...capability,
        state: 'validating',
        updatedAt: '2026-08-23T12:00:00.000Z',
        revision: 4,
      },
      upgrade: {
        ...upgrade,
        state: 'staging',
        evidence: { updaterState: 'checking_shadow' },
        updatedAt: '2026-08-23T12:00:00.000Z',
        revision: 4,
      },
      failures: [...failures].reverse(),
    });

    expect(first.handoffSha256).toBe(
      '5aaac97b0c76fa6308aab52f8701664e4132b5e404f947709157691ff83ac573',
    );
    expect(progressed).toEqual(first);
    expect(first.failureEvidence.map((failure) => failure.jobId)).toEqual([
      'aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa',
      'bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb',
    ]);
  });

  it('changes the hash when the failure evidence changes', () => {
    const first = capabilityHandoff({ capability, upgrade, failures });
    const changed = capabilityHandoff({
      capability,
      upgrade,
      failures: [
        ...failures,
        {
          id: 'cccccccc-cccc-4ccc-8ccc-cccccccccccc',
          capabilityKey: capability.key,
          normalizedCode: 'usepod_route_timed_out',
          jobClass: 'standard',
          occurredAt: '2026-08-22T11:45:00.000Z',
        },
      ],
    });

    expect(changed.handoffSha256).not.toBe(first.handoffSha256);
  });

  it('rejects an upgrade that belongs to another capability', () => {
    expect(() =>
      capabilityHandoff({
        capability,
        upgrade: { ...upgrade, capabilityId: 'different-capability' },
        failures,
      }),
    ).toThrow('upgrade is not bound to the capability');
  });
});
