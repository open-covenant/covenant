import { capabilityDescription, capabilityHandoff } from './capability-handoff.js';
import type { RescueBounty } from './domain/index.js';
import { publicCostCoverage } from './metrics.js';
import type { MizukiStore } from './store.js';
import { treasurySnapshot } from './treasury.js';
import type { ActivityEvent, Job, LedgerEntry } from './types.js';
import type { ServiceReadinessReport } from './readiness.js';

export function publicJob(job: Job) {
  return {
    id: job.id,
    state: job.state,
    issueUrl: job.quote.issueUrl,
    class: job.quote.class,
    priceAtomic: job.quote.priceAtomic,
    paymentTransaction: job.payment.transaction,
    prUrl: job.prUrl,
    mergedAt: job.mergedAt,
    refundTransaction: job.refundTransaction,
    refundOperationId: job.refundOperationId,
    error: publicFailure(job.error),
    changedFiles: job.artifacts?.changedFiles ?? [],
    validations:
      job.artifacts?.validations.map(({ command, exitCode }) => ({ command, exitCode })) ?? [],
    variableRouteCostEstimateUsd: job.estimatedCostUsd,
    costCoverage: publicCostCoverage,
    createdAt: job.createdAt,
    updatedAt: job.updatedAt,
  };
}

export async function publicBounty(store: MizukiStore, bounty: RescueBounty) {
  const [job, escrow, contributor] = await Promise.all([
    store.job(bounty.sourceJobId),
    store.escrowByBounty(bounty.id),
    bounty.activeClaim ? store.contributor(bounty.activeClaim.claimantId) : undefined,
  ]);
  const validationCommands = job?.quote.validationCommands ?? [];
  const acceptanceCriteria = [
    `Resolve issue #${bounty.issueNumber} as written by the maintainer`,
    ...(job ? [`Keep the patch within ${job.quote.maxFiles} changed files`] : []),
    ...validationCommands.map((command) => `Pass ${command}`),
    'Pass an independent patch review before payout',
  ];

  return {
    id: bounty.id,
    title: job?.quote.issueTitle ?? `Resolve issue #${bounty.issueNumber}`,
    repository: bounty.repository,
    issueUrl: bounty.issueUrl,
    issueNumber: bounty.issueNumber,
    amountUsd: bounty.priceCents / 100,
    amountAtomic: escrow?.amountAtomic,
    asset: 'SOL',
    state: bounty.state,
    failureClass: classifyFailure(job?.error),
    acceptanceCriteria,
    claimExpiresAt: bounty.activeClaim?.leaseExpiresAt,
    claimant: contributor ? { github: contributor.githubLogin } : undefined,
    escrowTransaction: escrow?.fundingSignature,
    releaseTransaction: escrow?.releaseSignature,
    refundTransaction: escrow?.refundSignature ?? job?.refundTransaction,
    pullRequestUrl: bounty.activeClaim?.draftPullRequestUrl,
    review: bounty.validationReceipt
      ? {
          approved: bounty.validationReceipt.approved,
          reason: bounty.validationReceipt.reason,
          reviewedAt: bounty.validationReceipt.reviewedAt,
        }
      : undefined,
    dispute: bounty.dispute
      ? {
          id: bounty.dispute.id,
          state: bounty.dispute.state,
          openedAt: bounty.dispute.openedAt,
          resolution: bounty.dispute.resolution
            ? {
                requestedDecision: bounty.dispute.resolution.requestedDecision,
                settlementDecision: bounty.dispute.resolution.settlementDecision,
                evidenceHash: bounty.dispute.resolution.evidenceHash,
                resolvedAt: bounty.dispute.resolution.resolvedAt,
              }
            : undefined,
        }
      : undefined,
    createdAt: bounty.createdAt,
    updatedAt: bounty.updatedAt,
  };
}

export async function publicTreasury(store: MizukiStore, readiness?: ServiceReadinessReport) {
  const [snapshot, ledger] = await Promise.all([
    treasurySnapshot(store, readiness),
    store.ledgerEntries(),
  ]);
  const dailyEstimateUsd = snapshot.trailingVariableAndOperatingEstimateUsd / 30;

  return {
    refundProtection: snapshot.refundProtection,
    recordedInflowsUsd: snapshot.recordedInflowsUsd,
    recordedOutflowsUsd: snapshot.recordedOutflowsUsd,
    recordedNetFlowUsd: snapshot.recordedNetFlowUsd,
    localOutstandingLiabilityUsd: snapshot.localOutstandingLiabilityUsd,
    plannedRunwayDays:
      dailyEstimateUsd > 0
        ? Math.floor(snapshot.allocationModel.operatingAllocationUsd / dailyEstimateUsd)
        : null,
    allocationModel: {
      ...snapshot.allocationModel,
      buckets: [
        {
          id: 'refund_target',
          label: 'Modeled refund allocation',
          allocatedUsd: snapshot.allocationModel.refundAllocationUsd,
          targetUsd: snapshot.allocationModel.refundTargetUsd,
        },
        {
          id: 'operating_target',
          label: 'Modeled operating allocation',
          allocatedUsd: snapshot.allocationModel.operatingAllocationUsd,
          targetUsd: snapshot.allocationModel.operatingTargetUsd,
        },
        {
          id: 'improvement_plan',
          label: 'Planned improvement allocation',
          allocatedUsd: snapshot.allocationModel.plannedImprovementAllocationUsd,
        },
        {
          id: 'research_plan',
          label: 'Planned route research allocation',
          allocatedUsd: snapshot.allocationModel.plannedResearchAllocationUsd,
        },
      ],
    },
    ledger: ledger.map(publicLedgerEntry),
    updatedAt: snapshot.updatedAt,
  };
}

export async function publicCapabilities(store: MizukiStore) {
  const [capabilities, upgrades] = await Promise.all([
    store.capabilitiesList(),
    store.upgradesList(),
  ]);
  const upgradeByCapability = new Map<string, (typeof upgrades)[number]>();
  for (const upgrade of upgrades) {
    const current = upgradeByCapability.get(upgrade.capabilityId);
    if (!current || upgrade.updatedAt > current.updatedAt) {
      upgradeByCapability.set(upgrade.capabilityId, upgrade);
    }
  }

  return capabilities.map((capability) => {
    const upgrade = upgradeByCapability.get(capability.id);
    return {
      id: capability.id,
      name: capability.name,
      description: capabilityDescription(capability.key),
      state: capability.state,
      category: capability.key.split('.')[0]?.replaceAll('-', ' ') ?? 'maintenance',
      upgradeId: upgrade?.id,
      upgradeState: upgrade?.state,
      handoffUrl: upgrade ? `/v1/capabilities/${capability.id}/handoff` : undefined,
      evidence: upgrade?.evidence,
      triggerReasons: upgrade?.triggerReasons ?? [],
      updatedAt: capability.updatedAt,
    };
  });
}

export async function publicCapabilityHandoff(store: MizukiStore, capabilityId: string) {
  const capability = (await store.capabilitiesList()).find((item) => item.id === capabilityId);
  if (!capability) return undefined;
  const upgrade = (await store.upgradesList())
    .filter((item) => item.capabilityId === capability.id)
    .sort((left, right) => right.updatedAt.localeCompare(left.updatedAt))[0];
  if (!upgrade) return undefined;
  const failures = await store.failuresForCapability(capability.key);
  return capabilityHandoff({ capability, upgrade, failures });
}

export async function publicActivityFeed(store: MizukiStore, limit = 100) {
  const events = await store.activity(limit);
  return Promise.all(events.map((event) => publicActivity(store, event)));
}

export async function publicActivity(store: MizukiStore, event: ActivityEvent) {
  const data = event.publicData;
  const job =
    event.kind.startsWith('job.') || event.kind.startsWith('refund.')
      ? await store.job(event.subjectId)
      : undefined;
  const bounty = event.kind.startsWith('bounty.') ? await store.bounty(event.subjectId) : undefined;
  const amountUsd = job
    ? Number(job.payment.amountAtomic) / 1_000_000
    : bounty
      ? bounty.priceCents / 100
      : undefined;
  const transaction =
    stringValue(data.transaction) ??
    stringValue(data.settlementTransaction) ??
    job?.refundTransaction;

  const presentation = activityPresentation(event.kind, job, bounty);
  return {
    id: event.id,
    kind: event.kind.replaceAll('.', '_'),
    title: presentation.title,
    description: presentation.description,
    ...(amountUsd === undefined ? {} : { amountUsd }),
    ...(presentation.href ? { href: presentation.href } : {}),
    ...(transaction ? { transaction } : {}),
    occurredAt: event.createdAt,
  };
}

function publicLedgerEntry(entry: LedgerEntry) {
  const presentation = ledgerPresentation(entry.kind);
  return {
    id: entry.id,
    type: presentation.type,
    direction: presentation.direction,
    description: presentation.description,
    ...(entry.asset === 'SOL'
      ? { amountAtomic: entry.amountAtomic, asset: entry.asset }
      : { amountUsd: entry.amountUsd }),
    transaction: entry.transaction,
    occurredAt: entry.createdAt,
  };
}

function ledgerPresentation(kind: LedgerEntry['kind']): {
  type:
    | 'customer_receipt'
    | 'platform_reported_creator_fee'
    | 'refund'
    | 'bounty_funding'
    | 'bounty_release'
    | 'route_cost'
    | 'operating_cost'
    | 'allocation';
  direction: 'credit' | 'debit' | 'allocation';
  description: string;
} {
  switch (kind) {
    case 'customer_payment':
      return {
        type: 'customer_receipt',
        direction: 'credit',
        description: 'Customer payment settlement receipt',
      };
    case 'creator_fee':
      return {
        type: 'platform_reported_creator_fee',
        direction: 'allocation',
        description: 'ClawPump-reported creator fee distribution (native SOL)',
      };
    case 'refund_completed':
      return { type: 'refund', direction: 'debit', description: 'Customer refunded in full' };
    case 'bounty_reserved':
      return {
        type: 'bounty_funding',
        direction: 'debit',
        description: 'Rescue bounty funded in signer-controlled SOL escrow',
      };
    case 'bounty_released':
      return {
        type: 'bounty_release',
        direction: 'allocation',
        description: 'Rescue escrow principal released to contributor',
      };
    case 'route_cost':
      return {
        type: 'route_cost',
        direction: 'debit',
        description: 'Variable execution cost estimate',
      };
    case 'operating_cost':
      return {
        type: 'operating_cost',
        direction: 'debit',
        description: 'Recorded operating cost',
      };
    case 'bounty_returned':
      return {
        type: 'allocation',
        direction: 'credit',
        description: 'Unused rescue escrow principal returned',
      };
    case 'treasury_deposit':
      return {
        type: 'allocation',
        direction: 'credit',
        description: 'Recorded treasury allocation entry (not custody proof)',
      };
    case 'refund_liability':
      return {
        type: 'allocation',
        direction: 'allocation',
        description: 'Application refund liability record',
      };
  }
}

function activityPresentation(
  kind: ActivityEvent['kind'],
  job: Job | undefined,
  bounty: RescueBounty | undefined,
): { title: string; description: string; href?: string } {
  const issue = job
    ? `${job.quote.owner}/${job.quote.repo} · issue #${job.quote.issueNumber}`
    : bounty
      ? `${bounty.repository} · issue #${bounty.issueNumber}`
      : 'Public maintenance work';
  switch (kind) {
    case 'job.paid':
      return {
        title: `${job?.quote.class === 'standard' ? 'Standard' : 'Micro'} job paid`,
        description: issue,
        href: job ? `/jobs/${job.id}` : undefined,
      };
    case 'job.delivered':
      return { title: 'Pull request delivered', description: issue, href: job?.prUrl };
    case 'job.failed':
      return {
        title: 'Paid attempt stopped',
        description: `${issue}; the full-refund process started.`,
      };
    case 'refund.pending':
      return {
        title: 'Full refund in progress',
        description: `${issue}; funds remain a recorded liability until finality.`,
      };
    case 'refund.completed':
      return {
        title: 'Full refund finalized',
        description: `${issue}; the customer received the complete payment back.`,
      };
    case 'bounty.created':
      return {
        title: 'Failure became a rescue bounty',
        description: issue,
        href: bounty ? `/bounties/${bounty.id}` : undefined,
      };
    case 'bounty.creation_failed':
      return {
        title: 'Bounty creation needs recovery',
        description: 'The refund completed; the separate rescue workflow is being retried.',
      };
    case 'bounty.funded':
      return {
        title: 'Rescue bounty opened',
        description: issue,
        href: bounty ? `/bounties/${bounty.id}` : undefined,
      };
    case 'bounty.claimed':
      return {
        title: 'Rescue bounty claimed',
        description: issue,
        href: bounty ? `/bounties/${bounty.id}` : undefined,
      };
    case 'bounty.pr_submitted':
      return {
        title: 'Rescue pull request submitted',
        description: issue,
        href: bounty?.activeClaim?.draftPullRequestUrl,
      };
    case 'bounty.accepted':
      return {
        title: 'Rescue patch accepted',
        description: `${issue}; payout is awaiting final release.`,
      };
    case 'bounty.released':
      return {
        title: 'Contributor escrow released',
        description: issue,
        href: bounty?.activeClaim?.draftPullRequestUrl,
      };
    case 'bounty.expired':
      return { title: 'Bounty escrow returned', description: `${issue}; this offer is closed.` };
    case 'bounty.disputed':
      return {
        title: 'Bounty dispute opened',
        description: `${issue}; payout is frozen pending resolution.`,
      };
    case 'bounty.dispute_resolved':
      return {
        title: 'Bounty dispute resolved',
        description: `${issue}; the escrow decision is finalized on-chain.`,
      };
    case 'capability.proposed':
      return {
        title: 'Capability upgrade proposed',
        description: 'A recurring failure created a benchmarked upgrade proposal.',
        href: '/capabilities',
      };
    case 'capability.activated':
      return {
        title: 'Capability upgrade activated',
        description: 'Independent evidence passed and the upgrade became active.',
        href: '/capabilities',
      };
    case 'capability.rolled_back':
      return {
        title: 'Capability upgrade rolled back',
        description: 'Post-deployment evidence failed the activation policy.',
        href: '/capabilities',
      };
  }
}

function classifyFailure(value: string | undefined): string {
  if (!value) return 'maintenance_failure';
  if (/route|model|inference|usepod/i.test(value)) return 'model_route';
  if (/review|quality|repair/i.test(value)) return 'independent_review';
  if (/validat|test|check/i.test(value)) return 'repository_validation';
  if (/scope|forbidden|too large|policy/i.test(value)) return 'scope_policy';
  if (/github|pull request|repository head/i.test(value)) return 'github_delivery';
  if (/timeout|timed out/i.test(value)) return 'execution_timeout';
  return 'maintenance_failure';
}

function publicFailure(value: string | undefined): string | undefined {
  if (!value) return undefined;
  switch (classifyFailure(value)) {
    case 'model_route':
      return 'The execution route did not complete reliably.';
    case 'independent_review':
      return 'The patch did not pass independent review.';
    case 'repository_validation':
      return 'The patch did not pass the repository validation commands.';
    case 'scope_policy':
      return 'The attempted patch exceeded the quoted scope policy.';
    case 'github_delivery':
      return 'The validated patch could not be delivered to GitHub.';
    case 'execution_timeout':
      return 'The bounded maintenance run timed out.';
    case 'maintenance_failure':
      return 'The maintenance run stopped before delivery.';
  }
}

function stringValue(value: unknown): string | undefined {
  return typeof value === 'string' && value.length > 0 ? value : undefined;
}
