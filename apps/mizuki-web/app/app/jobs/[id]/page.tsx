import type { Metadata } from 'next';
import { JobRoom } from '@/components/workbench/jobs';

export const metadata: Metadata = { title: 'Job room' };

export default async function JobRoomPage({ params }: { params: Promise<{ id: string }> }) {
  const { id } = await params;
  return <JobRoom id={id} />;
}
