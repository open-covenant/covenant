import { describe, expect, it } from 'vitest';
import {
  bountyPayoutText,
  isActiveJob,
  normalizeAccount,
  normalizeBounties,
  normalizeBilling,
  normalizeIssues,
  normalizeJobPage,
  normalizePreflight,
  normalizeRepositories,
  parseRepositoryLocator,
  type WorkbenchJob,
  workbenchAuthErrorMessage,
  workbenchAuthHref,
} from './workbench';

describe('Workbench authentication', () => {
  it('returns maintainers to the requested Workbench route', () => {
    expect(workbenchAuthHref('/app/jobs/new')).toBe(
      '/api/mizuki/v1/auth/github?return_to=%2Fapp%2Fjobs%2Fnew',
    );
  });

  it('rejects destinations outside Workbench', () => {
    expect(workbenchAuthHref('//example.com/steal')).toBe(
      '/api/mizuki/v1/auth/github?return_to=%2Fapp',
    );
    expect(workbenchAuthHref('/application')).toBe('/api/mizuki/v1/auth/github?return_to=%2Fapp');
  });

  it('shows only bounded OAuth failure messages', () => {
    expect(workbenchAuthErrorMessage('expired')).toContain('expired');
    expect(workbenchAuthErrorMessage('replayed')).toContain('already used');
    expect(workbenchAuthErrorMessage('internal database detail')).toBeUndefined();
    expect(workbenchAuthErrorMessage('toString')).toBeUndefined();
  });
});

describe('Workbench response normalization', () => {
  it('keeps failed and rejected jobs active until their refund is finalized', () => {
    for (const state of ['failed', 'rejected', 'refund_pending'] as const) {
      expect(isActiveJob({ state } as WorkbenchJob)).toBe(true);
    }
    for (const state of ['delivered', 'refunded'] as const) {
      expect(isActiveJob({ state } as WorkbenchJob)).toBe(false);
    }
  });

  it('reads the authenticated account contract', () => {
    expect(
      normalizeAccount({
        account: {
          githubId: '42',
          githubLogin: 'maintainer',
          wallet: 'wallet-address',
          walletVerifiedAt: '2026-08-25T08:00:00.000Z',
        },
      }),
    ).toEqual({
      githubLogin: 'maintainer',
      githubAvatarUrl: undefined,
      displayName: undefined,
      walletAddress: 'wallet-address',
    });
  });

  it('reads nested repository installation state without requiring list-level commands', () => {
    expect(
      normalizeRepositories({
        repositories: [
          {
            repository: 'open-covenant/covenant',
            readyForWork: true,
            core: { status: 'ready' },
            policy: { status: 'ready' },
            blockers: [],
          },
        ],
      }),
    ).toEqual([
      expect.objectContaining({
        fullName: 'open-covenant/covenant',
        readiness: 'ready',
        maintenanceAppStatus: 'installed',
        verifierAppStatus: 'installed',
        validationCommands: [],
      }),
    ]);
  });

  it('preserves repository and installation outages without requesting installation', () => {
    expect(
      normalizeRepositories({
        repositories: [
          {
            repository: 'open-covenant/covenant',
            readyForWork: false,
            verifierAppInstalled: false,
            core: { status: 'ready' },
            policy: { status: 'unavailable' },
            blockers: ['The policy verifier is temporarily unavailable.'],
          },
        ],
      })[0],
    ).toMatchObject({
      readiness: 'unavailable',
      maintenanceAppStatus: 'installed',
      verifierAppStatus: 'unavailable',
    });

    expect(
      normalizeRepositories({
        repositories: [
          {
            repository: 'open-covenant/covenant',
            readyForWork: false,
            core: { status: 'action_required' },
            policy: { status: 'ready' },
          },
        ],
      })[0],
    ).toMatchObject({
      readiness: 'action_required',
      maintenanceAppStatus: 'missing',
      verifierAppStatus: 'installed',
    });
  });

  it('does not treat an authorized but ineligible issue as ready', () => {
    expect(
      normalizeIssues({
        issues: [
          {
            number: 17,
            title: 'Add an unsupported feature',
            url: 'https://github.com/open-covenant/covenant/issues/17',
            authorized: true,
            eligibility: false,
            reason: 'This issue falls outside the supported maintenance scope.',
          },
        ],
      })[0],
    ).toMatchObject({
      authorized: true,
      eligibility: 'action_required',
      reason: 'This issue falls outside the supported maintenance scope.',
    });
  });

  it('maps a ready preflight with its nested repository record', () => {
    expect(
      normalizePreflight({
        readyForWork: true,
        repository: {
          repository: 'open-covenant/covenant',
          readyForWork: true,
          validationCommands: ['pnpm test'],
        },
        issue: {
          number: 18,
          title: 'Correct maintenance copy',
          url: 'https://github.com/open-covenant/covenant/issues/18',
          authorized: true,
          eligibility: true,
          class: 'micro',
        },
        checks: { eligibility: { status: 'ready' } },
        class: 'micro',
        maxFiles: 3,
        capturedHeadSha: 'a'.repeat(40),
      }),
    ).toMatchObject({
      repository: 'open-covenant/covenant',
      eligibility: 'ready',
      class: 'micro',
      maxFiles: 3,
      validationCommands: ['pnpm test'],
    });
  });

  it('preserves bounded job history metadata', () => {
    expect(
      normalizeJobPage({
        jobs: [],
        limit: 100,
        truncated: true,
        obligationCount: 4,
      }),
    ).toEqual({ jobs: [], limit: 100, truncated: true, obligationCount: 4 });
  });

  it('surfaces blockers from a preflight that is not ready', () => {
    expect(
      normalizePreflight({
        readyForWork: false,
        blockers: ['Policy verifier is not installed'],
        repository: { repository: 'open-covenant/covenant', readyForWork: false },
        issue: {
          number: 19,
          title: 'Correct a fixture',
          url: 'https://github.com/open-covenant/covenant/issues/19',
          authorized: true,
          eligibility: false,
        },
        checks: { eligibility: { status: 'action_required' } },
      }),
    ).toMatchObject({
      eligibility: 'action_required',
      reason: 'Policy verifier is not installed',
    });
  });

  it('keeps an eligible issue blocked when repository policy is not ready', () => {
    expect(
      normalizePreflight({
        readyForWork: false,
        blockers: ['Policy verifier status is unavailable'],
        repository: { repository: 'open-covenant/covenant' },
        issue: {
          number: 20,
          title: 'Correct a fixture',
          url: 'https://github.com/open-covenant/covenant/issues/20',
          authorized: true,
          eligibility: true,
        },
        checks: {
          policy: { status: 'unavailable' },
          eligibility: { status: 'ready' },
        },
      }),
    ).toMatchObject({
      eligibility: 'unavailable',
      reason: 'Policy verifier status is unavailable',
    });
  });

  it('accepts only exact GitHub repository locators', () => {
    expect(parseRepositoryLocator('open-covenant/covenant')).toEqual({
      owner: 'open-covenant',
      repo: 'covenant',
    });
    expect(parseRepositoryLocator('https://github.com/open-covenant/covenant.git')).toEqual({
      owner: 'open-covenant',
      repo: 'covenant',
    });
    expect(
      parseRepositoryLocator('https://github.com/open-covenant/covenant/issues/1'),
    ).toBeUndefined();
    expect(parseRepositoryLocator('https://example.com/open-covenant/covenant')).toBeUndefined();
  });

  it('reads both public and account bounty list envelopes', () => {
    const bounty = {
      id: 'bounty-1',
      title: 'Repair a test',
      repository: 'open-covenant/covenant',
      issueUrl: 'https://github.com/open-covenant/covenant/issues/20',
      amountUsd: 10,
      state: 'open',
      acceptanceCriteria: ['Tests pass'],
      createdAt: '2026-08-25T08:00:00.000Z',
      updatedAt: '2026-08-25T08:00:00.000Z',
    };
    expect(normalizeBounties({ bounties: [bounty] })).toEqual([bounty]);
    expect(normalizeBounties({ items: [bounty] })).toEqual([bounty]);
  });

  it('preserves bounded billing history metadata', () => {
    expect(
      normalizeBilling({
        transactions: [],
        limit: 1000,
        truncated: true,
        obligationCount: 3,
        totalsScope: 'latest_terminal_jobs_and_all_obligations',
      }),
    ).toMatchObject({
      entries: [],
      limit: 1000,
      truncated: true,
      obligationCount: 3,
      totalsScope: 'latest_terminal_jobs_and_all_obligations',
    });
  });

  it('renders bounty lamports as exact SOL and USD only as an approximation', () => {
    expect(bountyPayoutText('1234567890', 187.44)).toEqual({
      exact: '1.23456789 SOL',
      approximate: 'Approx. $187.44',
    });
    expect(bountyPayoutText(undefined, 20)).toEqual({
      exact: 'Exact SOL amount unavailable',
      approximate: 'Approx. $20',
    });
  });
});
