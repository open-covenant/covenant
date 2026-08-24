import type { Metadata } from 'next';
import { Jobs } from '@/components/workbench/jobs';

export const metadata: Metadata = { title: 'Jobs' };

export default function JobsPage() {
  return <Jobs />;
}
