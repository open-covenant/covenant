import type { Metadata } from 'next';
import { Settings } from '@/components/workbench/account-surfaces';

export const metadata: Metadata = { title: 'Settings' };

export default function SettingsPage() {
  return <Settings />;
}
