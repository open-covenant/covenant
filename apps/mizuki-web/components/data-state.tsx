import type { ReactNode } from 'react';

export function DataError({
  title = 'Live data unavailable',
  detail,
}: {
  title?: string;
  detail?: string;
}) {
  return (
    <div className="data-state data-error" role="status">
      <span className="data-state-mark" aria-hidden="true">
        !
      </span>
      <div>
        <strong>{title}</strong>
        <p>
          {detail ||
            'Mizuki could not reach the public API. This surface will retry when you refresh.'}
        </p>
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
  return <span className="demo-notice">Illustrative data</span>;
}
