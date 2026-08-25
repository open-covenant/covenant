import type { ProviderRouteReceipt } from '@/lib/types';

export function ProviderReceiptDetails({ receipt }: { receipt: ProviderRouteReceipt }) {
  return (
    <dl className="receipt-list provider-route-receipt">
      <div>
        <dt>Requested model</dt>
        <dd>{receipt.model}</dd>
      </div>
      {receipt.resolvedModel && receipt.resolvedModel !== receipt.model && (
        <div>
          <dt>Returned model</dt>
          <dd>{receipt.resolvedModel}</dd>
        </div>
      )}
      <div>
        <dt>Provider channel</dt>
        <dd>{receipt.route === 'marketplace' ? 'UsePod marketplace' : receipt.route}</dd>
      </div>
      {receipt.providerId && (
        <div>
          <dt>Provider</dt>
          <dd>
            <code>{receipt.providerId}</code>
          </dd>
        </div>
      )}
      {receipt.requestId && (
        <div>
          <dt>Request</dt>
          <dd>
            <code>{receipt.requestId}</code>
          </dd>
        </div>
      )}
      {receipt.costMicrounits && (
        <div>
          <dt>Marketplace-reported cost</dt>
          <dd>{formatMicrounits(receipt.costMicrounits)}</dd>
        </div>
      )}
    </dl>
  );
}

function formatMicrounits(value: string): string {
  if (!/^[0-9]+$/.test(value)) return 'Amount unavailable';
  const microunits = BigInt(value);
  const whole = microunits / 1_000_000n;
  const fraction = microunits % 1_000_000n;
  if (fraction === 0n) return `$${whole}`;
  return `$${whole}.${fraction.toString().padStart(6, '0').replace(/0+$/, '')}`;
}
