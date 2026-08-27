const compactNumber = new Intl.NumberFormat('en-US', {
  notation: 'compact',
  maximumFractionDigits: 1,
});

const dollars = new Intl.NumberFormat('en-US', {
  style: 'currency',
  currency: 'USD',
  minimumFractionDigits: 0,
  maximumFractionDigits: 2,
});

export function formatUsd(value: number): string {
  return dollars.format(value);
}

export function formatUsdcAtomic(value: string): string {
  const atomic = BigInt(value || '0');
  const whole = atomic / 1_000_000n;
  const fraction = atomic % 1_000_000n;
  if (fraction === 0n) return `${whole} USDC`;
  return `${whole}.${fraction.toString().padStart(6, '0').replace(/0+$/, '')} USDC`;
}

export function formatSolLamports(value: string): string {
  const atomic = BigInt(value || '0');
  const whole = atomic / 1_000_000_000n;
  const fraction = atomic % 1_000_000_000n;
  if (fraction === 0n) return `${whole} SOL`;
  return `${whole}.${fraction.toString().padStart(9, '0').replace(/0+$/, '')} SOL`;
}

export function formatPercent(value: number): string {
  return `${Math.round(value * 100)}%`;
}

export function formatCompact(value: number): string {
  return compactNumber.format(value);
}

export function formatTime(value: string): string {
  const date = new Date(value);
  if (Number.isNaN(date.getTime())) return 'Unknown time';
  return new Intl.DateTimeFormat('en', {
    month: 'short',
    day: 'numeric',
    year: 'numeric',
    hour: '2-digit',
    minute: '2-digit',
    hour12: false,
    timeZone: 'UTC',
    timeZoneName: 'short',
  }).format(date);
}

export function relativeTime(value: string, now = Date.now()): string {
  const timestamp = Date.parse(value);
  if (Number.isNaN(timestamp)) return 'unknown';
  const seconds = Math.round((timestamp - now) / 1_000);
  const formatter = new Intl.RelativeTimeFormat('en', { numeric: 'auto' });
  if (Math.abs(seconds) < 60) return formatter.format(seconds, 'second');
  const minutes = Math.round(seconds / 60);
  if (Math.abs(minutes) < 60) return formatter.format(minutes, 'minute');
  const hours = Math.round(minutes / 60);
  if (Math.abs(hours) < 24) return formatter.format(hours, 'hour');
  return formatter.format(Math.round(hours / 24), 'day');
}

export function truncateAddress(value: string, size = 5): string {
  if (value.length <= size * 2 + 1) return value;
  return `${value.slice(0, size)}…${value.slice(-size)}`;
}

export function stateLabel(value: string): string {
  const labels: Record<string, string> = {
    micro: 'Micro',
    standard: 'Standard',
    quoted: 'Quote created',
    settlement_pending: 'Payment pending',
    payment_expired: 'Payment authorization expired',
    paid: 'Payment confirmed',
    admitted: 'Payment confirmed',
    running: 'Work in progress',
    validating: 'Under validation',
    delivered: 'Pull request opened',
    rejected: 'Review failed',
    failed: 'Unable to deliver',
    refund_pending: 'Refund pending',
    refunded: 'Refund finalized',
    draft: 'Preparing bounty',
    awaiting_funding: 'Awaiting escrow funding',
    funding: 'Funding escrow',
    open: 'Open',
    claimed: 'Claimed',
    pr_submitted: 'Pull request submitted',
    claim_refund_pending: 'Escrow return pending',
    offer_refund_pending: 'Escrow return pending',
    release_refund_pending: 'Escrow return pending',
    accepted: 'Accepted',
    released: 'Paid',
    expired: 'Expired',
    disputed: 'Disputed',
    missing: 'Evidence unavailable',
    proposed: 'Proposed',
    funded: 'Funded',
    implementing: 'In development',
    active: 'Active',
    degraded: 'Needs attention',
    retired: 'Retired',
    verified: 'Verified',
    unavailable: 'Unavailable',
  };
  return labels[value] ?? 'Status unavailable';
}

export function bountyStateLabel(value: string): string {
  const labels: Record<string, string> = {
    draft: 'Preparing bounty',
    awaiting_funding: 'Awaiting escrow funding',
    funding: 'Funding escrow',
    open: 'Open',
    claimed: 'Claimed',
    pr_submitted: 'Pull request submitted',
    validating: 'Under validation',
    claim_refund_pending: 'SOL escrow return pending',
    offer_refund_pending: 'SOL escrow return pending',
    release_refund_pending: 'SOL escrow return pending',
    accepted: 'Payout approved',
    released: 'Payout completed',
    expired: 'Expired · SOL escrow returned',
    rejected: 'Contribution not accepted',
    disputed: 'Payout disputed',
    refunded: 'SOL escrow returned',
  };
  return labels[value] ?? 'Bounty status unavailable';
}

export function failureLabel(value?: string): string {
  if (!value) return 'Maintenance job not delivered';
  const labels: Record<string, string> = {
    maintenance_failure: 'Maintenance job not delivered',
    model_route: 'Required AI service did not complete',
    independent_review: 'Separate AI review did not approve the patch',
    repository_validation: 'Repository checks did not pass',
    scope_policy: 'Patch exceeded the authorized scope',
    github_delivery: 'GitHub delivery did not complete',
    execution_timeout: 'Maintenance job timed out',
    validation_failed: 'Repository checks did not pass',
    structured_edit: 'Structured edit',
    platform_specific_test: 'Platform-specific test',
  };
  return labels[value] ?? 'Maintenance job not delivered';
}
