import { describe, expect, it, vi } from 'vitest';
import type { IndependentReviewRequest } from './reviewer.js';
import { UsePodIndependentReviewer } from './reviewer.js';

const MODEL = 'deepseek-ai/DeepSeek-V3.2';

function config(
  overrides: Partial<ConstructorParameters<typeof UsePodIndependentReviewer>[0]> = {},
) {
  return {
    baseUrl: 'https://api.usepod.example',
    apiKey: 'review-key',
    model: MODEL,
    minimumBalance: '100',
    maxInputPriceMicrounits: 1_000_000,
    maxOutputPriceMicrounits: 1_000_000,
    maxCostMicrounits: 2_000_000,
    ...overrides,
  };
}

function request(
  diff = 'diff --git a/src/fix.ts b/src/fix.ts\n+return true;\n',
): IndependentReviewRequest {
  return {
    acceptanceHash: 'a'.repeat(64),
    reviewPolicyVersion: 1,
    evidence: {
      repository: 'owner/repository',
      issueNumber: 17,
      claimantGitHubLogin: 'contributor',
      pullRequestNumber: 23,
      pullRequestUrl: 'https://github.com/owner/repository/pull/23',
      mergeCommitOid: 'a'.repeat(40),
      headCommitOid: 'b'.repeat(40),
      baseCommitOid: 'd'.repeat(40),
      baseRefName: 'main',
      diffHash: 'c'.repeat(64),
      approvedReviewer: 'maintainer',
      approvedReviewSubmittedAt: '2026-08-22T12:04:00.000Z',
      checkCount: 2,
      createdAt: '2026-08-22T12:01:00.000Z',
      mergedAt: '2026-08-22T12:05:00.000Z',
    },
    artifact: {
      issueTitle: 'Handle empty input',
      issueBody: 'The parser should accept an empty input.',
      changedFiles: 1,
      diff,
    },
  };
}

function json(body: unknown, init: ResponseInit = {}): Response {
  return new Response(JSON.stringify(body), {
    ...init,
    headers: { 'content-type': 'application/json', ...init.headers },
  });
}

function approvedResponse(headers: Record<string, string> = {}): Response {
  return json(
    {
      model: MODEL,
      choices: [
        {
          index: 0,
          finish_reason: 'stop',
          message: {
            role: 'assistant',
            content: JSON.stringify({ approved: true, reason: 'The patch resolves the issue.' }),
          },
        },
      ],
      usage: { prompt_tokens: 96, completion_tokens: 63 },
    },
    {
      headers: {
        'x-pod-route': 'marketplace',
        'x-balance-remaining': '1000',
        ...headers,
      },
    },
  );
}

function streamedApprovedResponse(
  model = 'deepseek-v4-flash-0731',
  headers: Record<string, string> = {},
): Response {
  const decision = JSON.stringify({ approved: true, reason: 'The patch resolves the issue.' });
  const frames = [
    {
      id: 'request-7',
      model,
      choices: [
        {
          index: 0,
          delta: { role: 'assistant', content: '' },
          finish_reason: null,
        },
      ],
      usage: null,
    },
    {
      id: 'request-7',
      model,
      choices: [
        {
          index: 0,
          delta: { content: decision.slice(0, 20) },
          finish_reason: null,
        },
      ],
      usage: null,
    },
    {
      id: 'request-7',
      model,
      choices: [
        {
          index: 0,
          delta: { content: decision.slice(20) },
          finish_reason: null,
        },
      ],
      usage: null,
    },
    {
      id: 'request-7',
      model,
      choices: [
        {
          index: 0,
          delta: { content: '' },
          finish_reason: 'stop',
        },
      ],
      usage: null,
    },
    {
      model,
      choices: [],
      usage: { prompt_tokens: 96, completion_tokens: 63, total_tokens: 159 },
    },
  ];
  return new Response(
    `${frames.map((frame) => `data: ${JSON.stringify(frame)}\n\n`).join('')}data: [DONE]\n\n`,
    {
      headers: {
        'content-type': 'application/json',
        'x-pod-route': 'marketplace',
        'x-balance-remaining': '1000',
        ...headers,
      },
    },
  );
}

describe('funded independent review', () => {
  it('probes the exact model and funded balance without following redirects', async () => {
    const fetcher = vi.fn(async (input: string | URL | Request) =>
      String(input).endsWith('/models')
        ? json({ object: 'list', data: [{ id: MODEL }] })
        : json({ usdc_balance: '1000' }),
    ) as unknown as typeof fetch;
    const reviewer = new UsePodIndependentReviewer(config(), fetcher);

    await expect(reviewer.health()).resolves.toBeUndefined();
    expect(fetcher).toHaveBeenCalledTimes(2);
    for (const [, init] of vi.mocked(fetcher).mock.calls) {
      expect(init).toMatchObject({ method: 'GET', redirect: 'error' });
    }
    expect(vi.mocked(fetcher).mock.calls.map(([input]) => String(input))).toEqual([
      'https://api.usepod.example/proxy/review-key/v1/models',
      'https://api.usepod.example/proxy/review-key/balance',
    ]);
  });

  it('submits exact GitHub-fetched diff bytes under marketplace-only ceilings', async () => {
    const fetcher = vi.fn(async () => approvedResponse()) as unknown as typeof fetch;
    const reviewedAt = new Date('2026-08-22T12:03:00.000Z');
    const reviewer = new UsePodIndependentReviewer(config(), fetcher, () => reviewedAt);
    const input = request();

    const receipt = await reviewer.review(input);

    expect(receipt).toMatchObject({
      approved: true,
      model: MODEL,
      route: 'marketplace',
      reviewedAt: reviewedAt.toISOString(),
    });
    expect(receipt).not.toHaveProperty('providerId');
    expect(receipt).not.toHaveProperty('requestId');
    expect(receipt).not.toHaveProperty('costMicrounits');
    const [, init] = vi.mocked(fetcher).mock.calls[0]!;
    expect(init).toMatchObject({ method: 'POST', redirect: 'error' });
    expect(init?.headers).toMatchObject({
      'x-pod-routing-mode': 'marketplace-only',
      'x-pod-no-retention': 'true',
      'x-pod-max-price-input': '1000000',
      'x-pod-max-price-output': '1000000',
      'x-request-id': receipt.inputHash,
    });
    const body = JSON.parse(String(init?.body)) as {
      messages: { role: string; content: string }[];
      max_tokens: number;
    };
    expect(body.max_tokens).toBeGreaterThan(0);
    expect(body.max_tokens).toBeLessThanOrEqual(512);
    expect(JSON.parse(body.messages[1]!.content)).toMatchObject({ diff: input.artifact.diff });
  });

  it('sanitizes transport failures before they can expose the tokenized provider URL', async () => {
    const failing = vi.fn(async (input: string | URL | Request) => {
      throw new Error(`request failed for ${String(input)}`);
    }) as unknown as typeof fetch;
    const reviewer = new UsePodIndependentReviewer(config(), failing);

    for (const operation of [() => reviewer.health(), () => reviewer.review(request())]) {
      let failure: unknown;
      try {
        await operation();
      } catch (error) {
        failure = error;
      }
      expect(failure).toMatchObject({ code: 'independent_review_unavailable' });
      expect(String(failure)).not.toContain('review-key');
      expect(String(failure)).not.toContain('/proxy/');
    }
  });

  it('records optional provider, request, and bounded cost evidence when present', async () => {
    const fetcher = vi.fn(async () =>
      approvedResponse({
        'x-pod-provider-id': 'provider-7',
        'x-request-id': 'provider-request-7',
        'x-balance-cost-microunits': '500',
      }),
    ) as unknown as typeof fetch;
    const receipt = await new UsePodIndependentReviewer(config(), fetcher).review(request());

    expect(receipt).toMatchObject({
      providerId: 'provider-7',
      requestId: 'provider-request-7',
      costMicrounits: '500',
    });
  });

  it('accepts a complete streamed decision from a qualified resolved model', async () => {
    const fetcher = vi.fn(async () =>
      streamedApprovedResponse('deepseek/deepseek-v4-flash-0731', {
        'x-pod-provider-id': 'provider-7',
      }),
    ) as unknown as typeof fetch;
    const receipt = await new UsePodIndependentReviewer(
      config({ model: 'deepseek-v4-flash' }),
      fetcher,
    ).review(request());

    expect(receipt).toMatchObject({
      approved: true,
      model: 'deepseek-v4-flash',
      resolvedModel: 'deepseek/deepseek-v4-flash-0731',
      providerId: 'provider-7',
    });
  });

  it('fails closed on incomplete, inconsistent, or unqualified streams', async () => {
    const incomplete = vi.fn(async () => {
      const response = streamedApprovedResponse();
      return new Response((await response.text()).replace('data: [DONE]\n\n', ''), {
        headers: response.headers,
      });
    }) as unknown as typeof fetch;
    await expect(
      new UsePodIndependentReviewer(config({ model: 'deepseek-v4-flash' }), incomplete).review(
        request(),
      ),
    ).rejects.toMatchObject({ code: 'independent_review_unavailable', retryable: true });

    const inconsistent = vi.fn(async () => {
      const response = streamedApprovedResponse();
      return new Response(
        (await response.text()).replace(
          '"model":"deepseek-v4-flash-0731","choices":[]',
          '"model":"different-model","choices":[]',
        ),
        { headers: response.headers },
      );
    }) as unknown as typeof fetch;
    await expect(
      new UsePodIndependentReviewer(config({ model: 'deepseek-v4-flash' }), inconsistent).review(
        request(),
      ),
    ).rejects.toMatchObject({ code: 'independent_review_unavailable', retryable: true });

    const unqualified = vi.fn(async () =>
      streamedApprovedResponse('unqualified-model'),
    ) as unknown as typeof fetch;
    await expect(
      new UsePodIndependentReviewer(config({ model: 'deepseek-v4-flash' }), unqualified).review(
        request(),
      ),
    ).rejects.toMatchObject({ code: 'independent_review_unavailable', retryable: true });
  });

  it('rejects unaffordable or oversized input before making a paid call', async () => {
    const fetcher = vi.fn() as unknown as typeof fetch;
    await expect(
      new UsePodIndependentReviewer(config({ maxCostMicrounits: 1 }), fetcher).review(request()),
    ).rejects.toMatchObject({ code: 'independent_review_budget_exceeded' });
    await expect(
      new UsePodIndependentReviewer(config(), fetcher).review(request('x'.repeat(1_000_001))),
    ).rejects.toMatchObject({ code: 'independent_review_input_too_large' });
    expect(fetcher).not.toHaveBeenCalled();
  });

  it('fails closed on rejected decisions, route drift, or excessive reported cost', async () => {
    const rejected = vi.fn(async () =>
      json(
        {
          model: MODEL,
          choices: [
            {
              index: 0,
              finish_reason: 'stop',
              message: {
                role: 'assistant',
                content: JSON.stringify({ approved: false, reason: 'The issue remains open.' }),
              },
            },
          ],
          usage: { prompt_tokens: 96, completion_tokens: 63 },
        },
        { headers: { 'x-pod-route': 'marketplace', 'x-balance-remaining': '1000' } },
      ),
    ) as unknown as typeof fetch;
    await expect(
      new UsePodIndependentReviewer(config(), rejected).review(request()),
    ).rejects.toMatchObject({ code: 'independent_review_rejected', retryable: false });

    const wrongRoute = vi.fn(async () =>
      approvedResponse({ 'x-pod-route': 'direct' }),
    ) as unknown as typeof fetch;
    await expect(
      new UsePodIndependentReviewer(config(), wrongRoute).review(request()),
    ).rejects.toMatchObject({ code: 'independent_review_unavailable', retryable: true });

    const excessiveCost = vi.fn(async () =>
      approvedResponse({ 'x-balance-cost-microunits': '2000001' }),
    ) as unknown as typeof fetch;
    await expect(
      new UsePodIndependentReviewer(config(), excessiveCost).review(request()),
    ).rejects.toMatchObject({ code: 'independent_review_unavailable', retryable: true });
  });

  it('cancels a chunked response that exceeds the bounded receipt size', async () => {
    const body = new ReadableStream<Uint8Array>({
      start(controller) {
        controller.enqueue(new TextEncoder().encode('x'.repeat(65_537)));
        controller.close();
      },
    });
    const fetcher = vi.fn(
      async () =>
        new Response(body, {
          headers: {
            'content-type': 'application/json',
            'x-pod-route': 'marketplace',
            'x-balance-remaining': '1000',
          },
        }),
    ) as unknown as typeof fetch;

    await expect(
      new UsePodIndependentReviewer(config(), fetcher).review(request()),
    ).rejects.toMatchObject({ code: 'independent_review_unavailable', retryable: true });
  });
});
