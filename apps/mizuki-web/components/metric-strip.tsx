import { formatPercent, formatUsd } from '@/lib/format';
import type { Metrics } from '@/lib/types';

export function MetricStrip({ metrics }: { metrics: Metrics }) {
  const refundAttempts = metrics.refundCount + metrics.refundPending;
  const values: Array<{ label: string; value: string; highlight?: boolean; note?: string }> = [
    { label: 'Paid jobs', value: String(metrics.paidJobs) },
    { label: 'Pull requests', value: String(metrics.deliveredPrs) },
    { label: 'Merged', value: String(metrics.mergedPrs) },
    {
      label: 'Refund success',
      value:
        refundAttempts === 0 || metrics.refundSuccessRate === null
          ? 'No attempts'
          : formatPercent(metrics.refundSuccessRate),
      highlight: refundAttempts > 0 && metrics.refundPending === 0,
    },
    {
      label: 'Recognized revenue less partial est.',
      value: formatUsd(metrics.recognizedRevenueLessVariableRouteEstimateUsd),
      note: 'Revenue requires signer discharge; estimate includes model and sandbox, but excludes provider adjustments, chain/facilitator, and infrastructure',
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
