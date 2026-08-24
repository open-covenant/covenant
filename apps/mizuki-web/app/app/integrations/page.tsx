import type { Metadata } from 'next';
import { Integrations } from '@/components/workbench/account-surfaces';

export const metadata: Metadata = { title: 'Integrations' };

export default function IntegrationsPage() {
  return <Integrations />;
}
