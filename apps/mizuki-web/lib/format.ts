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
  if (fraction === 0n) return `$${whole}`;
  return `$${whole}.${fraction.toString().padStart(6, '0').replace(/0+$/, '')}`;
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
    hour: '2-digit',
    minute: '2-digit',
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
  return value.replaceAll('_', ' ');
}
