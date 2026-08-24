import type { Metadata } from 'next';
import { RepositoryWorkspace } from '@/components/workbench/repositories';

export const metadata: Metadata = { title: 'Repository' };

export default async function RepositoryPage({
  params,
}: {
  params: Promise<{ owner: string; repo: string }>;
}) {
  const { owner, repo } = await params;
  return <RepositoryWorkspace owner={owner} repo={repo} />;
}
