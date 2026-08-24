import type { Metadata } from 'next';
import { Repositories } from '@/components/workbench/repositories';

export const metadata: Metadata = { title: 'Repositories' };

export default function RepositoriesPage() {
  return <Repositories />;
}
