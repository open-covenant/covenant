import { renderToStaticMarkup } from 'react-dom/server';
import { describe, expect, it } from 'vitest';
import type { WorkbenchRepository } from '@/lib/workbench';
import { RepositoryCard, ServiceContractNote, WorkbenchEmpty } from './workbench-primitives';

describe('Workbench product copy', () => {
  it('states the exact service and refund contract', () => {
    const html = renderToStaticMarkup(<ServiceContractNote />);

    expect(html).toContain('Validated pull request or refund of the quoted USDC payment');
    expect(html).toContain('Mizuki cannot move refund funds');
    expect(html).toContain('Solana network and wallet fees are separate');
  });

  it('renders actionable repository readiness without inventing issue counts', () => {
    const repository: WorkbenchRepository = {
      owner: 'open-covenant',
      repo: 'covenant',
      fullName: 'open-covenant/covenant',
      readiness: 'ready',
      maintenanceAppInstalled: true,
      verifierAppInstalled: true,
      validationCommands: [],
    };
    const html = renderToStaticMarkup(<RepositoryCard repository={repository} />);

    expect(html).toContain('Ready');
    expect(html).toContain('Check repository');
    expect(html).not.toContain('0</dd>');
  });

  it('provides a useful zero state', () => {
    const html = renderToStaticMarkup(
      <WorkbenchEmpty title="No paid jobs yet" detail="Choose one authorized issue." />,
    );

    expect(html).toContain('No paid jobs yet');
    expect(html).toContain('Choose one authorized issue');
  });
});
