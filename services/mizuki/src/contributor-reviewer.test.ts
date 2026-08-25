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
  it('proves the configured model through the non-billable model catalog', async () => {
    const request = vi.fn<typeof fetch>(async (input, init) => {
      expect(String(input)).toBe('https://api.usepod.ai/proxy/secret/v1/models');
      expect(init?.method).toBe('GET');
      expect(init?.redirect).toBe('error');
      expect(init?.body).toBeUndefined();
      const headers = new Headers(init?.headers);
      expect(headers.get('authorization')).toBeNull();
      expect(headers.get('content-type')).toBeNull();
      expect(headers.get('x-pod-routing-mode')).toBeNull();
      expect(headers.get('x-pod-max-price-input')).toBeNull();
      return Response.json({
        object: 'list',
        data: [{ id: 'independent-reviewer', object: 'model' }],
      });
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

  it('rejects a catalog that does not contain the configured model', async () => {
    const reviewer = new UsePodContributorReviewer(
      loadConfig({
        MIZUKI_PAYMENT_MODE: 'mock',
        USEPOD_API_KEY: 'secret',
        USEPOD_REVIEW_MODEL: 'deepseek-v4-flash',
      }),
      {} as MizukiStore,
      {} as GithubClient,
      async () =>
        Response.json({
          object: 'list',
          data: [{ id: 'deepseek/deepseek-v4-flash-0731' }],
        }),
    );
    await expect(reviewer.readiness()).rejects.toThrow('does not include the configured model');
  });
});

describe('independent paid review', () => {
  it('returns a static rejection before any paid provider request', async () => {
    const request = vi.fn<typeof fetch>();
    const reviewer = paidReviewer(request, false);

    await expect(
      reviewer.preflight(reviewBounty, 'https://github.com/example/project/pull/23', {
        id: 'review-attempt-1',
        maxCostMicrounits: 1_000,
      }),
    ).resolves.toMatchObject({
      kind: 'rejected',
      result: { approved: false, reason: 'repository checks have not passed' },
    });
    expect(request).not.toHaveBeenCalled();
  });

  it('binds a bounded request and authoritative cost receipt to the reserved attempt', async () => {
    const request = vi.fn<typeof fetch>(async (input, init) => {
      expect(String(input)).toBe('https://api.usepod.ai/proxy/secret/v1/chat/completions');
      expect(init?.method).toBe('POST');
      expect(init?.redirect).toBe('error');
      const headers = new Headers(init?.headers);
      expect(headers.get('x-request-id')).toBe('review-attempt-1');
      expect(headers.get('x-pod-routing-mode')).toBe('marketplace-only');
      const body = JSON.parse(String(init?.body)) as { max_tokens: number };
      expect(body.max_tokens).toBeGreaterThan(0);
      expect(body.max_tokens).toBeLessThanOrEqual(512);
      return Response.json(
        {
          model: 'independent-reviewer',
          choices: [{ message: { content: '{"approved":true,"reason":"scoped fix"}' } }],
        },
        {
          headers: {
            'x-pod-route': 'marketplace',
            'x-balance-remaining': '9000000',
            'x-request-id': 'provider-request-9',
            'x-balance-cost-microunits': '450',
          },
        },
      );
    });
    const reviewer = paidReviewer(request);

    await expect(
      runPaidReview(reviewer, {
        id: 'review-attempt-1',
        maxCostMicrounits: 500,
      }),
    ).resolves.toMatchObject({
      approved: true,
      providerReceipt: {
        requestId: 'provider-request-9',
        costMicrounits: '450',
      },
    });
  });

  it('accepts a funded route without undocumented receipt headers', async () => {
    const reviewer = paidReviewer(async () =>
      Response.json(
        {
          model: 'independent-reviewer',
          choices: [{ message: { content: '{"approved":true,"reason":"scoped fix"}' } }],
        },
        {
          headers: {
            'x-pod-route': 'marketplace',
            'x-balance-remaining': '9000000',
          },
        },
      ),
    );

    await expect(
      runPaidReview(reviewer, {
        id: 'review-attempt-1',
        maxCostMicrounits: 1_000,
      }),
    ).resolves.toMatchObject({
      approved: true,
      providerReceipt: { model: 'independent-reviewer', route: 'marketplace' },
    });
  });

  it('accepts the exact canonical identity returned for the reviewer alias', async () => {
    const reviewer = paidReviewer(
      async () =>
        Response.json(
          {
            model: 'deepseek/deepseek-v4-flash-0731',
            choices: [{ message: { content: '{"approved":true,"reason":"scoped fix"}' } }],
          },
          {
            headers: {
              'x-pod-route': 'marketplace',
              'x-balance-remaining': '9000000',
            },
          },
        ),
      true,
      'deepseek-v4-flash',
    );

    await expect(
      runPaidReview(reviewer, {
        id: 'review-attempt-1',
        maxCostMicrounits: 1_000,
      }),
    ).resolves.toMatchObject({
      approved: true,
      providerReceipt: { model: 'deepseek-v4-flash', route: 'marketplace' },
    });
  });

  it('rejects a nearby canonical identity for the reviewer alias', async () => {
    const reviewer = paidReviewer(
      async () =>
        Response.json(
          {
            model: 'deepseek/deepseek-v4-flash-0730',
            choices: [{ message: { content: '{"approved":true,"reason":"scoped fix"}' } }],
          },
          {
            headers: {
              'x-pod-route': 'marketplace',
              'x-balance-remaining': '9000000',
            },
          },
        ),
      true,
      'deepseek-v4-flash',
    );

    await expect(
      runPaidReview(reviewer, {
        id: 'review-attempt-1',
        maxCostMicrounits: 1_000,
      }),
    ).rejects.toThrow('returned a different model');
  });

  it('rejects a reported provider cost above the reservation', async () => {
    const reviewer = paidReviewer(async () =>
      Response.json(
        {
          model: 'independent-reviewer',
          choices: [{ message: { content: '{"approved":true,"reason":"scoped fix"}' } }],
        },
        {
          headers: {
            'x-pod-route': 'marketplace',
            'x-balance-remaining': '9000000',
            'x-balance-cost-microunits': '1001',
          },
        },
      ),
    );

    await expect(
      runPaidReview(reviewer, {
        id: 'review-attempt-1',
        maxCostMicrounits: 1_000,
      }),
    ).rejects.toThrow('exceeded its reserved provider cost');
  });

  it('rejects an oversized paid-review response', async () => {
    const reviewer = paidReviewer(
      async () =>
        new Response('x'.repeat(64 * 1024 + 1), {
          headers: {
            'content-type': 'application/json',
            'x-pod-route': 'marketplace',
            'x-balance-remaining': '9000000',
          },
        }),
    );

    await expect(
      runPaidReview(reviewer, {
        id: 'review-attempt-1',
        maxCostMicrounits: 1_000,
      }),
    ).rejects.toThrow('response exceeded the size limit');
  });

  it.each([
    [{ id: '', maxCostMicrounits: 1_000 }, 'attempt ID'],
    [{ id: 'review-attempt-1', maxCostMicrounits: 0 }, 'cost reservation'],
    [{ id: 'review-attempt-1', maxCostMicrounits: 1.5 }, 'cost reservation'],
  ])('rejects an invalid attempt before any provider request %#', async (attempt, message) => {
    const request = vi.fn<typeof fetch>();
    const reviewer = paidReviewer(request);

    await expect(runPaidReview(reviewer, attempt)).rejects.toThrow(message);
    expect(request).not.toHaveBeenCalled();
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

const reviewBounty = {
  sourceJobId: 'job-1',
  repository: 'example/project',
} as RescueBounty;

async function runPaidReview(
  reviewer: UsePodContributorReviewer,
  attempt: { id: string; maxCostMicrounits: number },
) {
  const preflight = await reviewer.preflight(
    reviewBounty,
    'https://github.com/example/project/pull/23',
    attempt,
  );
  return preflight.kind === 'paid' ? reviewer.review(preflight) : preflight.result;
}

function paidReviewer(
  request: typeof fetch,
  checksPassed = true,
  model = 'independent-reviewer',
): UsePodContributorReviewer {
  const store = {
    job: async () => ({
      quote: {
        installationId: 7,
        maxFiles: 2,
        issueTitle: 'Fix the parser',
        issueBody: 'Reject invalid input.',
      },
    }),
  } as unknown as MizukiStore;
  const github = {
    pullRequestReviewData: async () => ({
      headSha: 'a'.repeat(40),
      baseSha: 'b'.repeat(40),
      baseRef: 'main',
      diffHash: 'c'.repeat(64),
      diff: 'diff --git a/src/parser.ts b/src/parser.ts',
      changedFiles: 1,
      files: [
        {
          filename: 'src/parser.ts',
          status: 'modified',
          patchAvailable: true,
        },
      ],
      mergedAt: null,
      mergeCommitSha: null,
      checksPassed,
      checkCount: 1,
    }),
  } as unknown as GithubClient;
  return new UsePodContributorReviewer(
    loadConfig({
      MIZUKI_PAYMENT_MODE: 'mock',
      USEPOD_API_KEY: 'secret',
      USEPOD_REVIEW_MODEL: model,
    }),
    store,
    github,
    request,
  );
}
