import type { ProviderRouteReceipt } from '@/lib/types';

export function ProviderReceiptDetails({ receipt }: { receipt: ProviderRouteReceipt }) {
  return (
    <dl className="receipt-list provider-route-receipt">
      <div>
        <dt>Model</dt>
        <dd>{receipt.model}</dd>
      </div>
      <div>
        <dt>Route</dt>
        <dd>{receipt.route}</dd>
      </div>
      {receipt.providerId && (
        <div>
          <dt>Provider ID</dt>
          <dd>
            <code>{receipt.providerId}</code>
          </dd>
        </div>
      )}
      {receipt.requestId && (
        <div>
          <dt>Request ID</dt>
          <dd>
            <code>{receipt.requestId}</code>
          </dd>
        </div>
      )}
      {receipt.costMicrounits && (
        <div>
          <dt>Provider-reported cost</dt>
          <dd>{receipt.costMicrounits} microunits</dd>
        </div>
      )}
    </dl>
  );
}
