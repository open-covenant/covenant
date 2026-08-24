import type { Metadata } from 'next';
import { Billing } from '@/components/workbench/billing';

export const metadata: Metadata = { title: 'Payments & refunds' };

export default function BillingPage() {
  return <Billing />;
}
