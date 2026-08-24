import type { ReactNode } from 'react';

export function DataError({
  title = 'Live records are temporarily unavailable',
  message = 'Refresh the page to try again. No action is required.',
}: {
  title?: string;
  message?: string;
}) {
  return (
    <div className="data-state data-error" role="status">
      <span className="data-state-mark" aria-hidden="true">
        !
      </span>
      <div>
        <strong>{title}</strong>
        <p>{message}</p>
      </div>
    </div>
  );
}

export function EmptyState({ title, children }: { title: string; children: ReactNode }) {
  return (
    <div className="data-state empty-state">
      <span className="data-state-mark" aria-hidden="true">
        0
      </span>
      <div>
        <strong>{title}</strong>
        <p>{children}</p>
      </div>
    </div>
  );
}

export function DemoNotice() {
  return <span className="demo-notice">Example data · not real transactions</span>;
}
