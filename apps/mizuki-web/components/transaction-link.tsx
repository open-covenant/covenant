import { truncateAddress } from '@/lib/format';

export function TransactionLink({
  signature,
  label = 'Transaction',
}: {
  signature: string;
  label?: string;
}) {
  const base = process.env.NEXT_PUBLIC_SOLANA_EXPLORER_URL || 'https://solscan.io';
  const cluster =
    process.env.NEXT_PUBLIC_SOLANA_NETWORK === 'solana-devnet' ? '?cluster=devnet' : '';
  return (
    <a
      href={`${base.replace(/\/$/, '')}/tx/${encodeURIComponent(signature)}${cluster}`}
      target="_blank"
      rel="noreferrer"
      className="receipt-link"
    >
      <span>{label}</span>
      <strong>{truncateAddress(signature, 6)} ↗</strong>
    </a>
  );
}
