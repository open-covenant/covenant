import { describe, expect, it, vi } from 'vitest';
import { loadConfig } from './config.js';
import {
  UsePodContributorReviewer,
  validateContributorFiles,
  validateRepositoryChecks,
} from './contributor-reviewer.js';
import type { RescueBounty } from './domain/index.js';
import type { GithubClient } from './github.js';
import type { MizukiStore } from './store.js';

describe('contributor file policy', () => {
  it('checks both sides of deletes and renames', () => {
    expect(
      validateContributorFiles(
        [{ filename: '.github/workflows/release.yml', status: 'removed', patchAvailable: true }],
        1,
      ),
    ).toMatchObject({ approved: false });
    expect(
      validateContributorFiles(
        [
          {
            filename: 'docs/release.yml',
            previousFilename: '.github/workflows/release.yml',
            status: 'renamed',
            patchAvailable: true,
          },
        ],
        1,
      ),
    ).toMatchObject({ approved: false });
  });

  it('rejects incomplete, binary, and truncated file evidence', () => {
    expect(validateContributorFiles([], 0)).toMatchObject({ approved: false });
    expect(validateContributorFiles([], 1)).toMatchObject({ approved: false });
    expect(
      validateContributorFiles(
        [{ filename: 'src/logo.png', status: 'modified', patchAvailable: false }],
        1,
      ),
    ).toMatchObject({ approved: false });
  });
});

describe('contributor repository checks', () => {
  it('requires at least one completed passing check', () => {
    expect(validateRepositoryChecks(0, true)).toMatchObject({ approved: false });
    expect(validateRepositoryChecks(1, false)).toMatchObject({ approved: false });
    expect(validateRepositoryChecks(1, true)).toMatchObject({ approved: true });
  });
});

describe('independent reviewer readiness', () => {
  it('proves the configured model through a funded marketplace completion', async () => {
    const request = vi.fn<typeof fetch>(async (input, init) => {
      expect(String(input)).toBe('https://api.usepod.ai/proxy/secret/v1/chat/completions');
      const headers = new Headers(init?.headers);
      expect(headers.get('authorization')).toBeNull();
      expect(headers.get('x-pod-routing-mode')).toBe('marketplace-only');
      expect(headers.get('x-pod-max-price-input')).toBe('200000');
      return Response.json(
        {
          model: 'independent-reviewer',
          choices: [{ message: { content: JSON.stringify({ nonce: 'mizuki-ready' }) } }],
        },
        {
          headers: {
            'x-pod-route': 'marketplace',
            'x-balance-remaining': '9000000',
          },
        },
      );
    });
    const reviewer = new UsePodContributorReviewer(
      loadConfig({
        MIZUKI_PAYMENT_MODE: 'mock',
        USEPOD_API_KEY: 'secret',
        USEPOD_REVIEW_MODEL: 'independent-reviewer',
      }),
      {} as MizukiStore,
      {} as GithubClient,
      request,
    );

    await expect(reviewer.readiness()).resolves.toBeUndefined();
  });

  it('rejects a completion that does not prove the configured model', async () => {
    const reviewer = new UsePodContributorReviewer(
      loadConfig({
        MIZUKI_PAYMENT_MODE: 'mock',
        USEPOD_API_KEY: 'secret',
        USEPOD_REVIEW_MODEL: 'independent-reviewer',
      }),
      {} as MizukiStore,
      {} as GithubClient,
      async () =>
        Response.json(
          {
            model: 'different-route',
            choices: [{ message: { content: JSON.stringify({ nonce: 'mizuki-ready' }) } }],
          },
          {
            headers: {
              'x-pod-route': 'marketplace',
              'x-balance-remaining': '9000000',
            },
          },
        ),
    );
    await expect(reviewer.readiness()).rejects.toThrow('different model');
  });
});

describe('merged review evidence', () => {
  it('returns the exact merged head and diff commitment from GitHub', async () => {
    const store = {
      job: async () => ({ quote: { installationId: 7 } }),
    } as unknown as MizukiStore;
    const github = {
      pullRequestReviewData: async () => ({
        headSha: 'a'.repeat(40),
        baseSha: 'd'.repeat(40),
        baseRef: 'main',
        diffHash: 'b'.repeat(64),
        diff: 'diff',
        changedFiles: 1,
        files: [],
        mergedAt: '2026-08-22T12:05:00.000Z',
        mergeCommitSha: 'c'.repeat(40),
        checksPassed: true,
        checkCount: 1,
      }),
    } as unknown as GithubClient;
    const reviewer = new UsePodContributorReviewer(
      loadConfig({ MIZUKI_PAYMENT_MODE: 'mock' }),
      store,
      github,
    );
    const bounty = {
      sourceJobId: 'job-1',
      repository: 'example/project',
    } as RescueBounty;

    await expect(
      reviewer.mergedEvidence(bounty, 'https://github.com/example/project/pull/23'),
    ).resolves.toEqual({
      headSha: 'a'.repeat(40),
      baseSha: 'd'.repeat(40),
      baseRef: 'main',
      diffHash: 'b'.repeat(64),
      mergedAt: '2026-08-22T12:05:00.000Z',
      mergeCommitSha: 'c'.repeat(40),
    });
  });
});
