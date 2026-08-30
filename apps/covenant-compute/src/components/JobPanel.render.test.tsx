import { renderToStaticMarkup } from 'react-dom/server';
import { describe, expect, it } from 'vitest';

import type { ComputeJob } from '../domain';
import { JobPanel } from './JobPanel';

const base: ComputeJob = {
  id: 'job-0123456789abcdef',
  app_id: 'gpu-workspace',
  offer_id: 'offer-1',
  status: 'provisioning',
  maximum_usdc_micros: 100_000,
  access_ready: false,
  error: null,
  receipt: null,
};

const noop = async () => {};

describe('job panel markup', () => {
  it('shows elapsed time and the long-wait line', () => {
    const markup = renderToStaticMarkup(
      <JobPanel
        busy={false}
        job={base}
        onCancel={noop}
        onOpen={noop}
        startedAt={Date.now() - 200_000}
      />,
    );
    expect(markup).toContain('Preparing your dedicated workspace');
    expect(markup).toContain('3:20 elapsed');
    expect(markup).toContain('longer than usual');
  });

  it('renders terminal copy and the unexplained failure fallback', () => {
    const markup = renderToStaticMarkup(
      <JobPanel
        busy={false}
        job={{ ...base, status: 'failed' }}
        onCancel={noop}
        onOpen={noop}
        startedAt={null}
      />,
    );
    expect(markup).toContain('Workload failed');
    expect(markup).toContain('did not report a reason');
    expect(markup).not.toContain('elapsed');
    expect(markup).not.toContain('job-progress');
  });

  it('keeps cancelled copy visible', () => {
    const markup = renderToStaticMarkup(
      <JobPanel
        busy={false}
        job={{ ...base, status: 'cancelled' }}
        onCancel={noop}
        onOpen={noop}
        startedAt={null}
      />,
    );
    expect(markup).toContain('Workload cancelled');
    expect(markup).toContain('job-status--cancelled');
  });
});
