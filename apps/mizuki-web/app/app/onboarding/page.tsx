import type { Metadata } from 'next';
import { RepositoryOnboarding } from '@/components/workbench/repositories';

export const metadata: Metadata = { title: 'Connect repository' };

export default function OnboardingPage() {
  return <RepositoryOnboarding />;
}
