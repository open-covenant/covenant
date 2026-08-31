import { renderToStaticMarkup } from 'react-dom/server';
import { describe, expect, it, vi } from 'vitest';
import type { WorkbenchPullRequest } from '@/lib/workbench';

const mocks = vi.hoisted(() => ({ useWorkbenchResource: vi.fn() }));

vi.mock('@/lib/workbench-client', () => ({
  useWorkbenchResource: mocks.useWorkbenchResource,
}));

import { RepositoryPullRequests } from './jobs';

const pullRequest: WorkbenchPullRequest = {
  repository: 'example/project',
  number: 114,
  title: 'bump the tooling group',
  url: 'https://github.com/example/project/pull/114',
  state: 'open',
  draft: false,
  authorized: false,
  author: 'dependabot[bot]',
  headRef: 'dependabot/tooling',
  headSha: 'a'.repeat(40),
  baseRef: 'main',
  createdAt: '2026-08-25T09:00:00.000Z',
  updatedAt: '2026-08-25T09:30:00.000Z',
  provenance: { kind: 'unlinked' },
};

function readyPage() {
  mocks.useWorkbenchResource.mockReturnValue({
    status: 'ready',
    refresh: vi.fn(),
    data: { pullRequests: [pullRequest], unavailableRepositories: [], truncated: false },
  });
}

describe('Repository pull requests', () => {
  it('offers pull request authorization on the repository surface', () => {
    readyPage();

    const html = renderToStaticMarkup(<RepositoryPullRequests />);

    expect(html).toContain('Authorize');
    expect(html).toContain('authorize_pr');
  });

  it('omits pull request authorization while requesting a quote', () => {
    readyPage();

    const html = renderToStaticMarkup(
      <RepositoryPullRequests repository="example/project" authorize={false} />,
    );

    expect(html).toContain('bump the tooling group');
    expect(html).not.toContain('authorize_pr');
    expect(html).toContain('fixed-quote maintenance starts from an authorized issue');
  });
});
