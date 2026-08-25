import { renderToStaticMarkup } from 'react-dom/server';
import { describe, expect, it } from 'vitest';
import type { WorkbenchRepository } from '@/lib/workbench';
import { ReadinessCheck } from './repositories';
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
      maintenanceAppStatus: 'installed',
      verifierAppStatus: 'installed',
      validationCommands: [],
    };
    const html = renderToStaticMarkup(<RepositoryCard repository={repository} />);

    expect(html).toContain('Ready');
    expect(html).toContain('Check repository');
    expect(html).not.toContain('0</dd>');
  });

  it('offers Retry for an outage and Install only for a missing App', () => {
    const unavailable = renderToStaticMarkup(
      <ReadinessCheck
        label="Policy verifier"
        status="unavailable"
        actionUrl="https://github.com/apps/example/installations/new"
        retry={() => undefined}
      />,
    );
    const missing = renderToStaticMarkup(
      <ReadinessCheck
        label="Policy verifier"
        status="missing"
        actionUrl="https://github.com/apps/example/installations/new"
        retry={() => undefined}
      />,
    );

    expect(unavailable).toContain('Status could not be confirmed');
    expect(unavailable).toContain('Retry');
    expect(unavailable).not.toContain('Install ↗');
    expect(missing).toContain('Required on this repository');
    expect(missing).toContain('Install ↗');
    expect(missing).not.toContain('Retry');
  });

  it('provides a useful zero state', () => {
    const html = renderToStaticMarkup(
      <WorkbenchEmpty title="No paid jobs yet" detail="Choose one authorized issue." />,
    );

    expect(html).toContain('No paid jobs yet');
    expect(html).toContain('Choose one authorized issue');
  });
});
