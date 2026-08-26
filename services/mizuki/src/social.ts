import { createHash } from 'node:crypto';
import type { Config } from './config.js';
import { metrics } from './metrics.js';
import type { ServiceReadinessReport } from './readiness.js';
import type { MizukiStore } from './store.js';
import type { Job, SocialPostKind, SocialPostReceipt, SocialStatsSnapshot } from './types.js';

const socialMetricKeys = [
  'internalPaidAttempts',
  'externalPaidJobs',
  'unclassifiedPaidAttempts',
  'internalOpenedPrs',
  'externalOpenedPrs',
  'unclassifiedOpenedPrs',
  'internalMergedPrs',
  'externalMergedPrs',
  'unclassifiedMergedPrs',
  'internalRefunds',
  'externalRefunds',
  'unclassifiedRefunds',
  'externalMaintainers',
] as const;

type SocialCountMetric = (typeof socialMetricKeys)[number];
type CountWithDelta = { total: number; delta: number };

export type SocialBrief = {
  schemaVersion: 1;
  generatedAt: string;
  freshUntil: string;
  cursor: string;
  sourceHash: string;
  kind: SocialPostKind;
  publishable: boolean;
  window: { from: string | null; to: string };
  metrics: Record<SocialCountMetric, CountWithDelta> & {
    refundSuccessRate: number | null;
    grossMarginStatus: 'unverified';
  };
  intake: { enabled: boolean };
  allowedUrlOrigins: string[];
  evidence: Array<{ claim: string; url: string }>;
  blockedReasons: string[];
  reviewRequiredReasons: string[];
  previousPostAt: string | null;
};

export async function buildSocialBrief(
  config: Config,
  store: MizukiStore,
  readiness: ServiceReadinessReport,
  kind: SocialPostKind = 'stats',
  now = new Date(),
): Promise<SocialBrief> {
  const [jobs, controls, receipts, publicMetrics] = await Promise.all([
    store.jobsList(),
    store.operatorControls(),
    store.socialPosts(100),
    metrics(config, store, readiness),
  ]);
  const snapshot = socialStatsSnapshot(config, jobs);
  const previous = receipts.find((receipt) => receipt.kind === kind);
  const allowedUrlOrigins = evidenceOrigins(config);
  const evidence = socialEvidence(config, jobs, allowedUrlOrigins);
  const sourceHash = hash({
    kind,
    snapshot,
    jobs: jobs
      .map((job) => ({
        id: job.id,
        state: job.state,
        version: job.version,
        updatedAt: job.updatedAt,
        prUrl: job.prUrl ?? null,
        mergedAt: job.mergedAt ?? null,
        provenance: jobProvenance(config, job),
      }))
      .sort((left, right) => left.id.localeCompare(right.id)),
    evidence,
  });
  const blockedReasons = blockedReasonsFor({
    snapshot,
    previous,
    sourceHash,
    refundProtectionVerified: publicMetrics.refundProtection.status === 'verified',
    aggregateConsistent:
      snapshot.internalPaidAttempts +
        snapshot.externalPaidJobs +
        snapshot.unclassifiedPaidAttempts ===
        publicMetrics.paidJobs &&
      snapshot.internalMergedPrs + snapshot.externalMergedPrs + snapshot.unclassifiedMergedPrs ===
        publicMetrics.mergedPrs &&
      snapshot.internalRefunds + snapshot.externalRefunds + snapshot.unclassifiedRefunds ===
        publicMetrics.refundCount &&
      snapshot.externalMaintainers === publicMetrics.externalMaintainers,
    completionEvidenceAvailable:
      (snapshot.internalMergedPrs + snapshot.externalMergedPrs + snapshot.unclassifiedMergedPrs ===
        0 ||
        evidence.some(({ claim }) => claim === 'mergedPr')) &&
      (snapshot.internalRefunds + snapshot.externalRefunds + snapshot.unclassifiedRefunds === 0 ||
        evidence.some(({ claim }) => claim === 'refund')),
  });

  return {
    schemaVersion: 1,
    generatedAt: now.toISOString(),
    freshUntil: new Date(now.getTime() + 15 * 60_000).toISOString(),
    cursor: `stats:${sourceHash.slice(0, 32)}`,
    sourceHash,
    kind,
    publishable: blockedReasons.length === 0,
    window: { from: previous?.postedAt ?? null, to: now.toISOString() },
    metrics: socialMetrics(snapshot, previous?.snapshot),
    intake: { enabled: controls.intakeEnabled },
    allowedUrlOrigins,
    evidence,
    blockedReasons,
    reviewRequiredReasons: ['financial_metrics'],
    previousPostAt: previous?.postedAt ?? null,
  };
}

export function socialStatsSnapshot(config: Config, jobs: Job[]): SocialStatsSnapshot {
  const paid = jobs.filter((job) => job.state !== 'settlement_pending');
  const opened = paid.filter((job) => job.prUrl);
  const merged = opened.filter((job) => job.mergedAt);
  const refunds = paid.filter((job) => job.state === 'refunded');
  const refundObligations = paid.filter((job) =>
    ['rejected', 'failed', 'refund_pending', 'refunded'].includes(job.state),
  );
  const groups = {
    paid: groupByProvenance(config, paid),
    opened: groupByProvenance(config, opened),
    merged: groupByProvenance(config, merged),
    refunds: groupByProvenance(config, refunds),
  };
  const externalMaintainers = new Set(
    groups.paid.external.flatMap((job) =>
      job.quote.authorizationReceipt ? [job.quote.authorizationReceipt.actorId] : [],
    ),
  );

  return {
    internalPaidAttempts: groups.paid.internal.length,
    externalPaidJobs: groups.paid.external.length,
    unclassifiedPaidAttempts: groups.paid.unclassified.length,
    internalOpenedPrs: groups.opened.internal.length,
    externalOpenedPrs: groups.opened.external.length,
    unclassifiedOpenedPrs: groups.opened.unclassified.length,
    internalMergedPrs: groups.merged.internal.length,
    externalMergedPrs: groups.merged.external.length,
    unclassifiedMergedPrs: groups.merged.unclassified.length,
    internalRefunds: groups.refunds.internal.length,
    externalRefunds: groups.refunds.external.length,
    unclassifiedRefunds: groups.refunds.unclassified.length,
    refundSuccessRate:
      refundObligations.length === 0 ? null : refunds.length / refundObligations.length,
    externalMaintainers: externalMaintainers.size,
    grossMarginStatus: 'unverified',
  };
}

export function snapshotFromSocialBrief(brief: SocialBrief): SocialStatsSnapshot {
  return {
    internalPaidAttempts: brief.metrics.internalPaidAttempts.total,
    externalPaidJobs: brief.metrics.externalPaidJobs.total,
    unclassifiedPaidAttempts: brief.metrics.unclassifiedPaidAttempts.total,
    internalOpenedPrs: brief.metrics.internalOpenedPrs.total,
    externalOpenedPrs: brief.metrics.externalOpenedPrs.total,
    unclassifiedOpenedPrs: brief.metrics.unclassifiedOpenedPrs.total,
    internalMergedPrs: brief.metrics.internalMergedPrs.total,
    externalMergedPrs: brief.metrics.externalMergedPrs.total,
    unclassifiedMergedPrs: brief.metrics.unclassifiedMergedPrs.total,
    internalRefunds: brief.metrics.internalRefunds.total,
    externalRefunds: brief.metrics.externalRefunds.total,
    unclassifiedRefunds: brief.metrics.unclassifiedRefunds.total,
    refundSuccessRate: brief.metrics.refundSuccessRate,
    externalMaintainers: brief.metrics.externalMaintainers.total,
    grossMarginStatus: brief.metrics.grossMarginStatus,
  };
}

function socialMetrics(
  current: SocialStatsSnapshot,
  previous?: SocialStatsSnapshot,
): SocialBrief['metrics'] {
  const counts = Object.fromEntries(
    socialMetricKeys.map((key) => [
      key,
      { total: current[key], delta: current[key] - (previous?.[key] ?? 0) },
    ]),
  ) as Record<SocialCountMetric, CountWithDelta>;
  return {
    ...counts,
    refundSuccessRate: current.refundSuccessRate,
    grossMarginStatus: current.grossMarginStatus,
  };
}

function blockedReasonsFor(input: {
  snapshot: SocialStatsSnapshot;
  previous?: SocialPostReceipt;
  sourceHash: string;
  refundProtectionVerified: boolean;
  aggregateConsistent: boolean;
  completionEvidenceAvailable: boolean;
}): string[] {
  const reasons: string[] = [];
  if (input.snapshot.unclassifiedPaidAttempts > 0) reasons.push('unclassified_paid_activity');
  if (!input.refundProtectionVerified) reasons.push('refund_protection_unverified');
  if (!input.aggregateConsistent) reasons.push('source_snapshot_mismatch');
  if (!input.completionEvidenceAvailable) reasons.push('completion_evidence_unavailable');
  if (input.previous?.sourceHash === input.sourceHash) reasons.push('duplicate_source');

  const changed = input.previous
    ? socialMetricKeys.some((key) => input.snapshot[key] !== input.previous?.snapshot[key]) ||
      input.snapshot.refundSuccessRate !== input.previous.snapshot.refundSuccessRate ||
      input.snapshot.grossMarginStatus !== input.previous.snapshot.grossMarginStatus
    : socialMetricKeys.some((key) => input.snapshot[key] !== 0) ||
      input.snapshot.refundSuccessRate !== null;
  if (!changed) reasons.push('no_changes_since_last_post');

  if (
    input.previous &&
    socialMetricKeys.some((key) => input.snapshot[key] < input.previous!.snapshot[key])
  ) {
    reasons.push('counter_regression');
  }
  return reasons;
}

function socialEvidence(
  config: Config,
  jobs: Job[],
  allowedUrlOrigins: string[],
): SocialBrief['evidence'] {
  const evidence: SocialBrief['evidence'] = [
    { claim: 'stats', url: publicApiUrl(config, '/v1/social/brief?kind=stats') },
  ];
  const merged = jobs
    .filter((job) => job.mergedAt && job.prUrl)
    .sort((left, right) => (right.mergedAt ?? '').localeCompare(left.mergedAt ?? ''))[0];
  if (merged?.prUrl && hasAllowedOrigin(merged.prUrl, allowedUrlOrigins)) {
    evidence.push({ claim: 'mergedPr', url: merged.prUrl });
  }
  const refunded = jobs
    .filter((job) => job.state === 'refunded')
    .sort((left, right) => right.updatedAt.localeCompare(left.updatedAt))[0];
  if (refunded) {
    evidence.push({
      claim: 'refund',
      url: publicApiUrl(config, `/v1/jobs/${encodeURIComponent(refunded.id)}/receipt`),
    });
  }
  return evidence;
}

function evidenceOrigins(config: Config): string[] {
  const origins = new Set(['https://github.com']);
  for (const value of [config.webOrigin, config.publicBaseUrl]) {
    if (!value) continue;
    try {
      origins.add(new URL(value).origin);
    } catch {
      continue;
    }
  }
  return [...origins].sort();
}

function hasAllowedOrigin(url: string, origins: string[]): boolean {
  try {
    return origins.includes(new URL(url).origin);
  } catch {
    return false;
  }
}

function publicApiUrl(config: Config, path: string): string {
  if (config.webOrigin) {
    return `${config.webOrigin.replace(/\/$/, '')}/api/mizuki${path}`;
  }
  return `${config.publicBaseUrl.replace(/\/$/, '')}${path}`;
}

function groupByProvenance(config: Config, jobs: Job[]) {
  return {
    internal: jobs.filter((job) => jobProvenance(config, job) === 'internal'),
    external: jobs.filter((job) => jobProvenance(config, job) === 'external'),
    unclassified: jobs.filter((job) => jobProvenance(config, job) === 'unclassified'),
  };
}

function jobProvenance(config: Config, job: Job): 'internal' | 'external' | 'unclassified' {
  const repository = `${job.quote.owner}/${job.quote.repo}`.toLowerCase();
  if (config.internalRepos.has(repository)) return 'internal';
  if (job.quote.installationId && job.quote.authorizationReceipt) return 'external';
  return 'unclassified';
}

function hash(value: unknown): string {
  return createHash('sha256').update(JSON.stringify(value)).digest('hex');
}
