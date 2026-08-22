import { stateLabel } from '@/lib/format';

const positive = new Set(['active', 'accepted', 'delivered', 'released', 'refunded']);
const warning = new Set(['awaiting_funding', 'disputed', 'expired', 'refund_pending', 'rollback']);
const working = new Set([
  'claimed',
  'funded',
  'implemented',
  'paid',
  'pr_submitted',
  'running',
  'validating',
]);

export function StatusBadge({ state }: { state: string }) {
  const tone = positive.has(state)
    ? 'positive'
    : warning.has(state)
      ? 'warning'
      : working.has(state)
        ? 'working'
        : 'neutral';
  return (
    <span className={`status-badge status-${tone}`}>
      <span className="status-dot" aria-hidden="true" />
      {stateLabel(state)}
    </span>
  );
}
