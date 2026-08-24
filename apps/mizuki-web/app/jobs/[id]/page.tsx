import type { Metadata } from 'next';
import { notFound } from 'next/navigation';
import { DataError, DemoNotice } from '@/components/data-state';
import { JobReceipt } from '@/components/job-receipt';
import { getJob } from '@/lib/api';

export const dynamic = 'force-dynamic';

export const metadata: Metadata = {
  title: 'Job status',
  description:
    'View payment, work, validation, delivery, and refund records for a Mizuki maintenance job.',
};

export default async function JobPage({ params }: { params: Promise<{ id: string }> }) {
  const { id } = await params;
  const result = await getJob(id);
  if (result.status === 'not_found') notFound();
  return (
    <div className="page-shell">
      <section className="page-hero compact-page-hero shell">
        <div>
          <p className="eyebrow">
            Public job record {result.status !== 'error' && result.demo && <DemoNotice />}
          </p>
          <h1>Track this job from payment to delivery or refund.</h1>
        </div>
        <p className="receipt-id">{id}</p>
      </section>
      <section className="shell receipt-section">
        {result.status === 'error' ? (
          <DataError title="This job record is temporarily unavailable" />
        ) : (
          <JobReceipt initial={result.data} live={!result.demo} />
        )}
      </section>
    </div>
  );
}
