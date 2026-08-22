import type { Metadata } from 'next';
import { DataError, DemoNotice } from '@/components/data-state';
import { JobReceipt } from '@/components/job-receipt';
import { getJob } from '@/lib/api';

export const dynamic = 'force-dynamic';

export const metadata: Metadata = {
  title: 'Job receipt',
  description:
    'Inspect payment, execution, validation, pull request, and refund evidence for a Mizuki job.',
};

export default async function JobPage({ params }: { params: Promise<{ id: string }> }) {
  const { id } = await params;
  const result = await getJob(id);
  return (
    <div className="page-shell">
      <section className="page-hero compact-page-hero shell">
        <div>
          <p className="eyebrow">
            Public job receipt {result.status !== 'error' && result.demo && <DemoNotice />}
          </p>
          <h1>Work should leave evidence.</h1>
        </div>
        <p className="receipt-id">{id}</p>
      </section>
      <section className="shell receipt-section">
        {result.status === 'error' ? (
          <DataError title="Job receipt unavailable" detail={result.error} />
        ) : (
          <JobReceipt initial={result.data} live={!result.demo} />
        )}
      </section>
    </div>
  );
}
