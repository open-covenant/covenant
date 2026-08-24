import { capabilityDescription, capabilityHandoff } from './capability-handoff.js';
import type { RescueBounty, RescueBountyState } from './domain/index.js';
import { publicCostCoverage } from './metrics.js';
import type { MizukiStore } from './store.js';
import { treasurySnapshot } from './treasury.js';
import type { ActivityEvent, Job, LedgerEntry, ProviderRouteReceipt } from './types.js';
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
    error: publicFailure(job.error),
    review: job.reviewReceipt
      ? {
          approved: job.reviewReceipt.approved,
          reason: publicReviewDecision(job.reviewReceipt.approved),
          reviewedAt: job.reviewReceipt.reviewedAt,
          artifactHash: job.reviewReceipt.artifactHash,
          ...(job.reviewReceipt.provider
            ? { provider: publicProviderReceipt(job.reviewReceipt.provider) }
            : {}),
        }
      : undefined,
    reviewAttempts: job.reviewAttempts?.map((attempt) => {
      const status = publicReviewStatus(attempt);
      return {
        phase: attempt.phase,
        status,
        artifactHash: attempt.artifactHash,
        reviewedAt: attempt.reviewedAt,
        costUsd: attempt.costUsd,
        ...(attempt.provider ? { provider: publicProviderReceipt(attempt.provider) } : {}),
        ...(attempt.approved === undefined ? {} : { approved: attempt.approved }),
        reason: publicReviewReason(status, attempt.approved),
      };
    }),
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
    'Pass a separate AI review before payout',
    'Receive approval from a non-claimant repository maintainer on the exact reviewed commit',
    'Merge the approved pull request before the 48-hour claim deadline',
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
    customerRefundTransaction: job?.refundTransaction,
    escrowReturnTransaction: escrow?.refundSignature,
    pullRequestUrl: bounty.activeClaim?.draftPullRequestUrl,
    review: bounty.validationReceipt
      ? {
          approved: bounty.validationReceipt.approved,
          reason: publicReviewDecision(bounty.validationReceipt.approved),
          reviewedAt: bounty.validationReceipt.reviewedAt,
          headSha: bounty.validationReceipt.headSha,
          baseSha: bounty.validationReceipt.baseSha,
          baseRef: bounty.validationReceipt.baseRef,
          diffHash: bounty.validationReceipt.diffHash,
          ...(bounty.validationReceipt.provider
            ? { provider: publicProviderReceipt(bounty.validationReceipt.provider) }
            : {}),
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
                summary: bounty.dispute.resolution.evidence.summary,
                references: [...bounty.dispute.resolution.evidence.references],
                evidenceHash: bounty.dispute.resolution.evidenceHash,
                decidedAt: bounty.dispute.resolution.decidedAt,
                resolvedAt: bounty.dispute.resolution.resolvedAt,
                transactionSignature: bounty.dispute.resolution.transactionSignature,
              }
            : undefined,
        }
      : undefined,
    createdAt: bounty.createdAt,
    updatedAt: bounty.updatedAt,
  };
}

const publicBountyStates = new Set<RescueBountyState>([
  'open',
  'claimed',
  'pr_submitted',
  'validating',
  'claim_refund_pending',
  'offer_refund_pending',
  'release_refund_pending',
  'accepted',
  'released',
  'expired',
  'rejected',
  'disputed',
  'refunded',
]);

export async function isPublicBounty(store: MizukiStore, bounty: RescueBounty): Promise<boolean> {
  if (!publicBountyStates.has(bounty.state)) return false;
  const [escrow, job] = await Promise.all([
    store.escrowByBounty(bounty.id),
    store.job(bounty.sourceJobId),
  ]);
  return Boolean(
    job?.state === 'refunded' &&
    job.refundTransaction &&
    escrow?.fundingSignature &&
    escrow.amountAtomic &&
    /^[0-9]+$/.test(escrow.amountAtomic) &&
    BigInt(escrow.amountAtomic) > 0n,
  );
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
          label: 'Planned refund allocation',
          allocatedUsd: snapshot.allocationModel.refundAllocationUsd,
          targetUsd: snapshot.allocationModel.refundTargetUsd,
        },
        {
          id: 'operating_target',
          label: 'Planned operating allocation',
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
          label: 'Planned provider research allocation',
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
  const published = await Promise.all(events.map((event) => publicActivity(store, event)));
  return published.filter((event): event is NonNullable<typeof event> => Boolean(event));
}

export async function publicActivity(store: MizukiStore, event: ActivityEvent) {
  if (event.kind === 'bounty.created' || event.kind === 'bounty.creation_failed') return undefined;
  const data = event.publicData;
  const job =
    event.kind.startsWith('job.') || event.kind.startsWith('refund.')
      ? await store.job(event.subjectId)
      : undefined;
  const bounty = event.kind.startsWith('bounty.') ? await store.bounty(event.subjectId) : undefined;
  if (event.kind.startsWith('bounty.') && (!bounty || !(await isPublicBounty(store, bounty)))) {
    return undefined;
  }
  if (event.kind === 'bounty.funded') {
    const escrow = await store.escrowByBounty(event.subjectId);
    if (!escrow?.fundingSignature || stringValue(data.transaction) !== escrow.fundingSignature) {
      return undefined;
    }
  }
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
  if (!presentation) return undefined;
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
    | 'bounty_return'
    | 'route_cost'
    | 'operating_cost'
    | 'refund_obligation'
    | 'treasury_deposit'
    | 'allocation';
  direction: 'credit' | 'debit' | 'allocation';
  description: string;
} {
  switch (kind) {
    case 'customer_payment':
      return {
        type: 'customer_receipt',
        direction: 'credit',
        description: 'Customer payment finalized',
      };
    case 'creator_fee':
      return {
        type: 'platform_reported_creator_fee',
        direction: 'allocation',
        description: 'ClawPump-reported creator fee distribution (native SOL)',
      };
    case 'refund_completed':
      return { type: 'refund', direction: 'debit', description: 'Quoted USDC payment refunded' };
    case 'bounty_reserved':
      return {
        type: 'bounty_funding',
        direction: 'debit',
        description: 'Maintenance bounty funded in dedicated SOL escrow',
      };
    case 'bounty_released':
      return {
        type: 'bounty_release',
        direction: 'allocation',
        description: 'Maintenance-bounty escrow released to the contributor',
      };
    case 'route_cost':
      return {
        type: 'route_cost',
        direction: 'debit',
        description: 'Tracked model and sandbox cost estimate',
      };
    case 'operating_cost':
      return {
        type: 'operating_cost',
        direction: 'debit',
        description: 'Recorded operating cost',
      };
    case 'bounty_returned':
      return {
        type: 'bounty_return',
        direction: 'credit',
        description: 'Unused maintenance-bounty escrow returned',
      };
    case 'treasury_deposit':
      return {
        type: 'treasury_deposit',
        direction: 'credit',
        description: 'Planning allocation recorded — not proof of funds',
      };
    case 'refund_liability':
      return {
        type: 'refund_obligation',
        direction: 'allocation',
        description: 'Outstanding refund obligation recorded',
      };
  }
}

function activityPresentation(
  kind: ActivityEvent['kind'],
  job: Job | undefined,
  bounty: RescueBounty | undefined,
): { title: string; description: string; href?: string } | undefined {
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
      return { title: 'Pull request opened', description: issue, href: job?.prUrl };
    case 'job.failed':
      return {
        title: 'Paid attempt stopped',
        description: `${issue}; Mizuki could not open a qualifying pull request, so the refund process started.`,
      };
    case 'refund.pending':
      return {
        title: 'Full refund in progress',
        description: `${issue}; the quoted USDC payment remains protected until the refund is final.`,
      };
    case 'refund.completed':
      return {
        title: 'Full refund finalized',
        description: `${issue}; 100% of the quoted USDC payment was returned to the original payer.`,
      };
    case 'bounty.created':
    case 'bounty.creation_failed':
      return undefined;
    case 'bounty.funded':
      return {
        title: 'Funded bounty published',
        description: `${issue}; the SOL payout is now in on-chain escrow.`,
        href: bounty ? `/bounties/${bounty.id}` : undefined,
      };
    case 'bounty.claimed':
      return {
        title: 'Maintenance bounty claimed',
        description: issue,
        href: bounty ? `/bounties/${bounty.id}` : undefined,
      };
    case 'bounty.pr_submitted':
      return {
        title: 'Maintenance pull request submitted',
        description: issue,
        href: bounty?.activeClaim?.draftPullRequestUrl,
      };
    case 'bounty.accepted':
      return {
        title: 'Merged maintenance patch verified',
        description: `${issue}; payout verification is in progress.`,
      };
    case 'bounty.released':
      return {
        title: 'Bounty payout released',
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
        description:
          bounty?.dispute?.resolution?.settlementDecision === 'release'
            ? `${issue}; the contributor payout finalized on-chain.`
            : `${issue}; the SOL escrow return finalized on-chain.`,
        href: bounty ? `/bounties/${bounty.id}` : undefined,
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
        description:
          'Required benchmark, code-change, review, and deployment records were verified before activation.',
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
      return 'A required AI service did not complete the work.';
    case 'independent_review':
      return 'The separate AI review did not approve the patch.';
    case 'repository_validation':
      return 'The patch did not pass the repository checks.';
    case 'scope_policy':
      return 'The attempted changes exceeded the quoted scope.';
    case 'github_delivery':
      return 'The validated patch could not be delivered to GitHub.';
    case 'execution_timeout':
      return 'The work took too long and stopped before delivery.';
    case 'maintenance_failure':
      return 'Work stopped before a pull request was opened.';
  }
}

function publicProviderReceipt(receipt: ProviderRouteReceipt): ProviderRouteReceipt {
  return {
    model: receipt.model,
    route: receipt.route,
    ...(receipt.providerId ? { providerId: receipt.providerId } : {}),
    ...(receipt.requestId ? { requestId: receipt.requestId } : {}),
    ...(receipt.costMicrounits ? { costMicrounits: receipt.costMicrounits } : {}),
  };
}

function publicReviewStatus(attempt: NonNullable<Job['reviewAttempts']>[number]) {
  if (attempt.status) return attempt.status;
  if (attempt.error) return 'failed' as const;
  if (attempt.approved !== undefined) return 'completed' as const;
  if (attempt.provider) return 'received' as const;
  return 'pending' as const;
}

function publicReviewReason(
  status: ReturnType<typeof publicReviewStatus>,
  approved: boolean | undefined,
): string {
  if (status === 'failed') return 'The separate AI review could not be completed.';
  if (status === 'pending') return 'The separate AI review is in progress.';
  if (status === 'received') {
    return 'The AI provider response was recorded; a final review decision is not yet available.';
  }
  return publicReviewDecision(approved === true);
}

function publicReviewDecision(approved: boolean): string {
  return approved
    ? 'The separate AI review approved the patch against the issue scope and repository checks.'
    : 'The separate AI review did not approve the patch against the issue scope and repository checks.';
}

function stringValue(value: unknown): string | undefined {
  return typeof value === 'string' && value.length > 0 ? value : undefined;
}
