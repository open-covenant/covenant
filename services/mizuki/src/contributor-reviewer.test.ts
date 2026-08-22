import { describe, expect, it, vi } from 'vitest';
import { loadConfig } from './config.js';
import {
  UsePodContributorReviewer,
  validateContributorFiles,
  validateRepositoryChecks,
} from './contributor-reviewer.js';
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
  it('authenticates and requires the configured marketplace model', async () => {
    const request = vi.fn<typeof fetch>(async (_input, init) => {
      expect(init?.headers).toMatchObject({
        authorization: 'Bearer secret',
        'x-pod-routing-mode': 'marketplace-only',
      });
      return Response.json({ data: [{ id: 'independent-reviewer' }] });
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

  it('rejects a successful but incomplete model catalog', async () => {
    const reviewer = new UsePodContributorReviewer(
      loadConfig({
        MIZUKI_PAYMENT_MODE: 'mock',
        USEPOD_API_KEY: 'secret',
        USEPOD_REVIEW_MODEL: 'independent-reviewer',
      }),
      {} as MizukiStore,
      {} as GithubClient,
      async () => Response.json({ data: [{ id: 'different-route' }] }),
    );
    await expect(reviewer.readiness()).rejects.toThrow('unavailable');
  });
});
