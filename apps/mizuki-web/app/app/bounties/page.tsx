import type { Metadata } from 'next';
import { BountyWorkspace } from '@/components/workbench/bounty-workspace';

export const metadata: Metadata = { title: 'Bounty workspace' };

export default function BountiesPage() {
  return <BountyWorkspace />;
}
