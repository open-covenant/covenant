import type { Metadata } from 'next';
import { NewJobWizard } from '@/components/workbench/new-job-wizard';

export const metadata: Metadata = { title: 'New maintenance job' };

export default async function NewJobPage({
  searchParams,
}: {
  searchParams: Promise<{ owner?: string; repo?: string; issue?: string }>;
}) {
  const query = await searchParams;
  const issue = query.issue && /^\d+$/.test(query.issue) ? Number(query.issue) : undefined;
  return <NewJobWizard initialOwner={query.owner} initialRepo={query.repo} initialIssue={issue} />;
}
