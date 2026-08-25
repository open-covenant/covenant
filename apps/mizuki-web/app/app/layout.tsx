import type { Metadata } from 'next';
import { WorkbenchShell } from '@/components/workbench/workbench-shell';
import '../workbench.css';

export const metadata: Metadata = {
  title: 'Workbench',
  description:
    'Manage authorized repositories, fixed-price maintenance jobs, pull requests, refunds, and funded bounties.',
  robots: { index: false, follow: false },
  alternates: { canonical: '/app' },
};

export default function AppLayout({ children }: { children: React.ReactNode }) {
  return <WorkbenchShell>{children}</WorkbenchShell>;
}
