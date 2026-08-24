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
          title="Paid issue intake unavailable"
          detail={`Mizuki could not verify that intake is open (${admission.error}). Quote and payment controls stay disabled.`}
        />
      ) : !admission.data.intakeEnabled ? (
        <EmptyState title="Paid issue intake is closed">
          New quotes and payments are paused. Existing public job receipts remain available.
        </EmptyState>
      ) : (
        <>
          {admission.demo && (
            <p className="eyebrow intake-demo">
              Illustrative intake <DemoNotice />
            </p>
          )}
          {children}
        </>
      )}
    </div>
  );
}
