import type { ReactNode } from 'react';
import { DataError, DemoNotice, EmptyState } from './data-state';
import type { Admission, Loadable } from '@/lib/types';

export function IntakeGate({
  admission,
  children,
}: {
  admission: Loadable<Admission>;
  children: ReactNode;
}) {
  return (
    <div className="work-intake">
      {admission.status === 'error' ? (
        <DataError
          title="Issue submission is temporarily unavailable"
          message="Quote and payment controls remain unavailable until service status can be confirmed. Refresh the page to try again."
        />
      ) : !admission.data.intakeEnabled ? (
        <EmptyState title="New paid jobs are temporarily paused">
          Quotes and payments are currently unavailable. Existing public job records remain
          available.
        </EmptyState>
      ) : (
        <>
          {admission.demo && (
            <p className="eyebrow intake-demo">
              Example quote flow <DemoNotice />
            </p>
          )}
          {children}
        </>
      )}
    </div>
  );
}
