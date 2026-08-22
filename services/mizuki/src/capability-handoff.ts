import { createHash } from 'node:crypto';
import type { Capability, FailureRecord, Upgrade } from './domain/index.js';

export type CapabilityHandoffBody = {
  version: 1;
  proposalId: string;
  capability: {
    id: string;
    key: string;
    name: string;
    description: string;
  };
  triggerReasons: string[];
  failureEvidence: Array<{
    jobId: string;
    normalizedCode: string;
    jobClass: FailureRecord['jobClass'];
    occurredAt: string;
  }>;
  benchmarkContract: {
    suite: string;
    targetMetric: string;
    direction: 'increase' | 'decrease';
  };
  protectedContracts: string[];
  authorityBoundary: {
    handoffIsAuthorization: false;
    proposalAuthority: 'external_release_authority';
    benchmarkAuthority: 'external_benchmark_authority';
    reviewAuthority: 'external_review_authority';
    submission: 'Mizuki Updater POST /v1/upgrades';
    note: string;
  };
  createdAt: string;
};

export type CapabilityHandoff = CapabilityHandoffBody & { handoffSha256: string };

export function buildCapabilityHandoff(input: {
  capability: Capability;
  upgrade: Upgrade;
  failures: readonly FailureRecord[];
}): CapabilityHandoffBody {
  const { capability, upgrade } = input;
  if (upgrade.capabilityId !== capability.id) {
    throw new Error('upgrade is not bound to the capability');
  }

  return {
    version: 1,
    proposalId: upgrade.id,
    capability: {
      id: capability.id,
      key: capability.key,
      name: capability.name,
      description: capabilityDescription(capability.key),
    },
    triggerReasons: [...upgrade.triggerReasons].sort(),
    failureEvidence: input.failures
      .filter((failure) => failure.capabilityKey === capability.key)
      .map((failure) => ({
        jobId: failure.id,
        normalizedCode: failure.normalizedCode,
        jobClass: failure.jobClass,
        occurredAt: failure.occurredAt,
      }))
      .sort(
        (left, right) =>
          left.occurredAt.localeCompare(right.occurredAt) || left.jobId.localeCompare(right.jobId),
      ),
    benchmarkContract: benchmarkContract(capability.key),
    protectedContracts: [
      'paid job admission and delivery',
      'full-principal refund liability and recovery',
      'signer-controlled SOL rescue escrow',
      'independent policy-signer boundaries',
    ],
    authorityBoundary: {
      handoffIsAuthorization: false,
      proposalAuthority: 'external_release_authority',
      benchmarkAuthority: 'external_benchmark_authority',
      reviewAuthority: 'external_review_authority',
      submission: 'Mizuki Updater POST /v1/upgrades',
      note: 'Mizuki publishes this unsigned handoff but cannot sign or submit an upgrade.',
    },
    createdAt: upgrade.createdAt,
  };
}

export function capabilityHandoff(input: {
  capability: Capability;
  upgrade: Upgrade;
  failures: readonly FailureRecord[];
}): CapabilityHandoff {
  const body = buildCapabilityHandoff(input);
  return { ...body, handoffSha256: hashCapabilityHandoff(body) };
}

export function hashCapabilityHandoff(handoff: CapabilityHandoffBody): string {
  return createHash('sha256').update(canonicalJson(handoff)).digest('hex');
}

export function capabilityDescription(key: string): string {
  const descriptions: Record<string, string> = {
    'model.route-reliability': 'Choose a reliable coding route under the fixed job cost ceiling.',
    'patch.quality':
      'Produce focused patches that pass independent review without unsafe expansion.',
    'repository.validation':
      'Discover and run the repository checks that cover the requested maintenance work.',
    'scope.classification':
      'Reject risky work before payment and keep accepted jobs within fixed limits.',
    'github.delivery': 'Deliver one consented pull request against the quoted repository revision.',
    'execution.timeout': 'Complete bounded maintenance runs within the public service deadline.',
    'maintenance.general':
      'Improve reliable completion of small public repository maintenance work.',
  };
  return descriptions[key] ?? 'Improve a measured part of the public maintenance workflow.';
}

function benchmarkContract(key: string): CapabilityHandoffBody['benchmarkContract'] {
  const metrics: Record<string, string> = {
    'model.route-reliability': 'paid_job_delivery_rate',
    'patch.quality': 'independent_review_acceptance_rate',
    'repository.validation': 'repository_validation_pass_rate',
    'scope.classification': 'unsafe_issue_rejection_rate',
    'github.delivery': 'authorized_pr_delivery_rate',
    'execution.timeout': 'bounded_completion_rate',
    'maintenance.general': 'paid_job_delivery_rate',
  };
  return {
    suite: 'mizuki-commercial-core',
    targetMetric: metrics[key] ?? 'paid_job_delivery_rate',
    direction: 'increase',
  };
}

function canonicalJson(value: unknown): string {
  if (value === null || typeof value === 'string' || typeof value === 'boolean') {
    return JSON.stringify(value);
  }
  if (typeof value === 'number') {
    if (!Number.isFinite(value)) throw new Error('canonical handoff contains a non-finite number');
    return JSON.stringify(value);
  }
  if (Array.isArray(value)) return `[${value.map(canonicalJson).join(',')}]`;
  if (typeof value !== 'object') throw new Error('canonical handoff contains an invalid value');
  const object = value as Record<string, unknown>;
  return `{${Object.keys(object)
    .filter((key) => object[key] !== undefined)
    .sort()
    .map((key) => `${JSON.stringify(key)}:${canonicalJson(object[key])}`)
    .join(',')}}`;
}
