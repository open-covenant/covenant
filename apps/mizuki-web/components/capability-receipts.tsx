import { truncateAddress } from '@/lib/format';
import type { Capability } from '@/lib/types';

export function CapabilityReceipts({ capability }: { capability: Capability }) {
  const evidence = capability.evidence;
  const receipts = [
    evidence?.benchmarkReceiptId
      ? { label: 'Benchmark', value: evidence.benchmarkReceiptId }
      : undefined,
    evidence?.reviewReceiptId ? { label: 'Review', value: evidence.reviewReceiptId } : undefined,
    evidence?.updaterAuditHash ? { label: 'Audit', value: evidence.updaterAuditHash } : undefined,
    evidence?.manifestHash ? { label: 'Manifest', value: evidence.manifestHash } : undefined,
    evidence?.deploymentId ? { label: 'Deployment', value: evidence.deploymentId } : undefined,
    evidence?.promotionOperationId
      ? { label: 'Promotion', value: evidence.promotionOperationId }
      : undefined,
  ].filter((receipt): receipt is { label: string; value: string } => Boolean(receipt));
  const pullRequestUrl = githubUrl(evidence?.pullRequestUrl ?? capability.evidenceUrl);

  if (!pullRequestUrl && receipts.length === 0) return null;

  return (
    <div className="capability-receipts" aria-label="Capability evidence receipts">
      {pullRequestUrl && (
        <a href={pullRequestUrl} target="_blank" rel="noreferrer">
          Pull request <span aria-hidden="true">↗</span>
        </a>
      )}
      {receipts.map((receipt) => (
        <span title={receipt.value} key={`${receipt.label}-${receipt.value}`}>
          {receipt.label} {truncateAddress(receipt.value, 7)}
        </span>
      ))}
    </div>
  );
}

function githubUrl(value: string | undefined): string | undefined {
  if (!value) return undefined;
  try {
    const url = new URL(value);
    return url.protocol === 'https:' && url.hostname === 'github.com' ? url.toString() : undefined;
  } catch {
    return undefined;
  }
}
