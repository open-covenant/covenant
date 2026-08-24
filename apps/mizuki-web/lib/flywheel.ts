import { formatPercent, formatUsd } from './format';
import type { Metrics } from './types';

export type TractionTarget = {
  id: 'paid-jobs' | 'delivered-prs' | 'merged-prs' | 'external-maintainers' | 'refunds' | 'margin';
  label: string;
  value: string;
  target: string;
  detail: string;
  met: boolean;
  progress: number;
};

export function tractionTargets(metrics: Metrics): TractionTarget[] {
  const refundAttempts = metrics.refundCount + metrics.refundPending;
  const refundTargetMet =
    refundAttempts > 0 && metrics.refundPending === 0 && metrics.refundSuccessRate === 1;
  const excludedCosts = metrics.costCoverage.excluded.map(costCategoryLabel).join(', ');

  return [
    countTarget('paid-jobs', 'Paid jobs', metrics.paidJobs, 10, 'Finalized customer payments'),
    countTarget(
      'delivered-prs',
      'Delivered pull requests',
      metrics.deliveredPrs,
      7,
      'Validated patches published',
    ),
    countTarget(
      'merged-prs',
      'Merged pull requests',
      metrics.mergedPrs,
      5,
      'Accepted by maintainers',
    ),
    countTarget(
      'external-maintainers',
      'External maintainers',
      metrics.externalMaintainers,
      3,
      `${metrics.externalRepositories} external repos with a paid, App-authorized job`,
    ),
    {
      id: 'refunds',
      label: 'Successful refunds',
      value:
        refundAttempts === 0 || metrics.refundSuccessRate === null
          ? 'No attempts'
          : formatPercent(metrics.refundSuccessRate),
      target: '100%',
      detail:
        refundAttempts === 0
          ? 'A finalized refund canary is still required'
          : `${metrics.refundCount} finalized · ${metrics.refundPending} pending`,
      met: refundTargetMet,
      progress:
        refundAttempts === 0 || metrics.refundSuccessRate === null
          ? 0
          : Math.min(100, metrics.refundSuccessRate * 100),
    },
    {
      id: 'margin',
      label: 'Gross margin',
      value: 'Unverified',
      target: 'Above $0',
      detail: `${formatUsd(metrics.recognizedRevenueLessVariableRouteEstimateUsd)} recognized revenue after recorded model and sandbox estimates; excludes ${excludedCosts}`,
      met: false,
      progress: 0,
    },
  ];
}

function costCategoryLabel(value: Metrics['costCoverage']['excluded'][number]): string {
  switch (value) {
    case 'provider_billing_adjustments':
      return 'provider billing adjustments';
    case 'chain_and_facilitator_fees':
      return 'chain/facilitator fees';
    case 'infrastructure':
      return 'infrastructure';
  }
}

function countTarget(
  id: TractionTarget['id'],
  label: string,
  current: number,
  target: number,
  detail: string,
): TractionTarget {
  return {
    id,
    label,
    value: String(current),
    target: String(target),
    detail,
    met: current >= target,
    progress: Math.min(100, (current / target) * 100),
  };
}
