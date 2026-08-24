import { formatPercent, formatUsd } from '@/lib/format';
import type { Metrics } from '@/lib/types';

export function MetricStrip({ metrics }: { metrics: Metrics }) {
  const refundAttempts = metrics.refundCount + metrics.refundPending;
  const values: Array<{ label: string; value: string; highlight?: boolean; note?: string }> = [
    { label: 'Paid jobs', value: String(metrics.paidJobs) },
    { label: 'Pull requests opened', value: String(metrics.deliveredPrs) },
    { label: 'Pull requests merged', value: String(metrics.mergedPrs) },
    {
      label: 'Refund completion',
      value:
        refundAttempts === 0 || metrics.refundSuccessRate === null
          ? 'Not yet measured'
          : formatPercent(metrics.refundSuccessRate),
      highlight: refundAttempts > 0 && metrics.refundPending === 0,
    },
    {
      label: 'Revenue after tracked variable costs',
      value: formatUsd(metrics.recognizedRevenueLessVariableRouteEstimateUsd),
      note: 'Estimate only. Includes recorded AI model and sandbox costs. Provider billing adjustments, Solana and payment fees, and hosting are still excluded, so gross margin is not verified.',
    },
  ];

  return (
    <dl className="metric-strip">
      {values.map((item) => (
        <div className={item.highlight ? 'metric-highlight' : ''} key={item.label}>
          <dt>{item.label}</dt>
          <dd>{item.value}</dd>
          {item.note && <small>{item.note}</small>}
        </div>
      ))}
    </dl>
  );
}
