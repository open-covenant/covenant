import type { Metadata } from 'next';
import { BountyRoom } from '@/components/workbench/bounty-workspace';

export const metadata: Metadata = { title: 'Bounty workspace' };

export default async function BountyPage({ params }: { params: Promise<{ id: string }> }) {
  const { id } = await params;
  return <BountyRoom id={id} />;
}
