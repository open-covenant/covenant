import { ContributorPatchReviewError } from './bounties.js';
import type {
  ContributorPatchPaidPreflight,
  ContributorPatchReviewAccounting,
  ContributorPatchReviewAttempt,
  ContributorPatchReviewer,
} from './bounties.js';
import type { Config } from './config.js';
import type { RescueBounty } from './domain/index.js';
import { GithubClient, parsePullRequestUrl } from './github.js';
import type { MizukiStore } from './store.js';
import {
  boundedMaxTokens,
  matchesUsePodModel,
  parseUsePodReviewDecision,
  parseUsePodUsage,
  probeUsePodCatalog,
  publicUsePodReceipt,
  readUsePodChatCompletion,
  usePodHeaders,
  usePodReceipt,
  usePodUrl,
} from './usepod.js';

const MAX_REVIEW_OUTPUT_TOKENS = 512;
const forbiddenPath =
  /(^|\/)(\.github\/workflows|\.env|secrets?|vendor|generated|dist|build|node_modules)(\/|$)|(^|\/)(package-lock\.json|pnpm-lock\.yaml|yarn\.lock)$/i;

export class UsePodContributorReviewer implements ContributorPatchReviewer {
  constructor(
    private readonly config: Config,
    private readonly store: MizukiStore,
    private readonly github: GithubClient,
    private readonly request: typeof fetch = fetch,
  ) {}

  async readiness(): Promise<void> {
    if (!this.config.usePodApiKey || !this.config.usePodModel) {
      throw new Error('independent reviewer route is not configured');
    }
    await probeUsePodCatalog(this.requestConfig(), this.request);
  }

  async preflight(
    bounty: RescueBounty,
    pullRequestUrl: string,
    attempt: ContributorPatchReviewAttempt,
  ) {
    if (!/^[A-Za-z0-9][A-Za-z0-9._:-]{0,127}$/.test(attempt.id)) {
      throw new Error('bounty review attempt ID is invalid');
    }
    if (!Number.isSafeInteger(attempt.maxCostMicrounits) || attempt.maxCostMicrounits <= 0) {
      throw new Error('bounty review cost reservation is invalid');
    }
    const job = await this.store.job(bounty.sourceJobId);
    if (!job?.quote.installationId)
      throw new Error('source repository installation is unavailable');
    const parsed = parsePullRequestUrl(pullRequestUrl);
    if (`${parsed.owner}/${parsed.repo}`.toLowerCase() !== bounty.repository) {
      throw new Error('pull request repository does not match bounty');
    }
    const data = await this.github.pullRequestReviewData(pullRequestUrl, job.quote.installationId);
    const evidence = {
      headSha: data.headSha,
      baseSha: data.baseSha,
      baseRef: data.baseRef,
      diffHash: data.diffHash,
    };
    if (data.changedFiles > job.quote.maxFiles) {
      return {
        kind: 'rejected' as const,
        result: {
          approved: false as const,
          reason: `change exceeds ${job.quote.maxFiles}-file scope`,
          ...evidence,
        },
      };
    }
    const filePolicy = validateContributorFiles(data.files, data.changedFiles);
    if (!filePolicy.approved) {
      return {
        kind: 'rejected' as const,
        result: { approved: false as const, reason: filePolicy.reason, ...evidence },
      };
    }
    const checkPolicy = validateRepositoryChecks(data.checkCount, data.checksPassed);
    if (!checkPolicy.approved) {
      return {
        kind: 'rejected' as const,
        result: { approved: false as const, reason: checkPolicy.reason, ...evidence },
      };
    }
    if (!this.config.usePodApiKey) throw new Error('USEPOD_API_KEY is required for bounty review');
    const requestConfig = this.requestConfig();
    const providerInput = {
      model: this.config.usePodModel,
      issue: { title: job.quote.issueTitle, body: job.quote.issueBody },
      diff: data.diff,
      repositoryChecks: { count: data.checkCount, passed: data.checksPassed },
    };
    const maxTokens = boundedMaxTokens(
      reviewRequest(providerInput, MAX_REVIEW_OUTPUT_TOKENS),
      attempt.maxCostMicrounits,
      requestConfig.maxInputPriceMicrounits,
      requestConfig.maxOutputPriceMicrounits,
      MAX_REVIEW_OUTPUT_TOKENS,
    );
    return {
      kind: 'paid' as const,
      attempt,
      evidence,
      providerInput: { ...providerInput, maxOutputTokens: maxTokens },
    };
  }

  async review(
    preflight: ContributorPatchPaidPreflight,
    checkpoint?: (accounting: ContributorPatchReviewAccounting) => Promise<void>,
  ) {
    const requestConfig = this.requestConfig();
    let response: Response;
    try {
      response = await this.request(usePodUrl(requestConfig, 'chat/completions'), {
        method: 'POST',
        redirect: 'error',
        headers: { ...usePodHeaders(requestConfig), 'x-request-id': preflight.attempt.id },
        body: JSON.stringify(
          reviewRequest(preflight.providerInput, preflight.providerInput.maxOutputTokens),
        ),
        signal: AbortSignal.timeout(60_000),
      });
    } catch {
      throw new Error('UsePod bounty review request failed');
    }
    const completion = await readUsePodChatCompletion(response);
    const receipt = usePodReceipt(
      response,
      preflight.providerInput.model,
      requestConfig.minimumBalance,
      completion.model,
    );
    const providerReceipt = publicUsePodReceipt(receipt);
    let usage: ReturnType<typeof parseUsePodUsage> | undefined;
    if (completion.usage !== undefined) {
      try {
        usage = parseUsePodUsage(completion.usage);
      } catch {
        usage = undefined;
      }
    }
    const accounting: ContributorPatchReviewAccounting = {
      providerReceipt,
      ...(usage ? { inputTokens: usage.promptTokens, outputTokens: usage.completionTokens } : {}),
    };
    if (checkpoint) {
      try {
        await checkpoint(accounting);
      } catch {
        providerReviewFailure('UsePod bounty review receipt persistence failed', accounting);
      }
    }
    if (
      receipt.costMicrounits &&
      BigInt(receipt.costMicrounits) > BigInt(preflight.attempt.maxCostMicrounits)
    ) {
      providerReviewFailure('UsePod bounty review exceeded its reserved provider cost', accounting);
    }
    if (!response.ok) {
      providerReviewFailure(`UsePod bounty review failed with HTTP ${response.status}`, accounting);
    }
    if (completion.model && !matchesUsePodModel(preflight.providerInput.model, completion.model)) {
      providerReviewFailure('UsePod bounty review returned a different model', accounting);
    }
    if (!usage) {
      providerReviewFailure(
        completion.ok ? 'UsePod bounty review returned invalid token usage' : completion.error,
        accounting,
      );
    }
    if (!completion.ok) providerReviewFailure(completion.error, accounting);
    if (!matchesUsePodModel(preflight.providerInput.model, completion.model)) {
      providerReviewFailure('UsePod bounty review returned a different model', accounting);
    }
    if (usage.completionTokens > preflight.providerInput.maxOutputTokens) {
      providerReviewFailure('UsePod bounty review exceeded its output token ceiling', accounting);
    }
    let decision;
    try {
      decision = parseUsePodReviewDecision(completion.content);
    } catch {
      providerReviewFailure('UsePod bounty review returned an invalid decision', accounting);
    }
    return {
      ...decision,
      ...preflight.evidence,
      providerReceipt,
      inputTokens: usage.promptTokens,
      outputTokens: usage.completionTokens,
    };
  }

  async mergedEvidence(bounty: RescueBounty, pullRequestUrl: string) {
    const job = await this.store.job(bounty.sourceJobId);
    if (!job?.quote.installationId) {
      throw new Error('source repository installation is unavailable');
    }
    const parsed = parsePullRequestUrl(pullRequestUrl);
    if (`${parsed.owner}/${parsed.repo}`.toLowerCase() !== bounty.repository) {
      throw new Error('pull request repository does not match bounty');
    }
    const data = await this.github.pullRequestReviewData(pullRequestUrl, job.quote.installationId);
    if (!data.mergedAt || !data.mergeCommitSha) {
      throw new Error('pull request is not merged');
    }
    return {
      headSha: data.headSha,
      baseSha: data.baseSha,
      baseRef: data.baseRef,
      diffHash: data.diffHash,
      mergedAt: data.mergedAt,
      mergeCommitSha: data.mergeCommitSha,
    };
  }

  private requestConfig() {
    return {
      baseUrl: this.config.usePodBaseUrl,
      token: this.config.usePodApiKey,
      model: this.config.usePodModel,
      maxInputPriceMicrounits: this.config.usePodMaxInputPriceMicrounits,
      maxOutputPriceMicrounits: this.config.usePodMaxOutputPriceMicrounits,
      minimumBalance: this.config.usePodMinimumBalance,
    };
  }
}

function providerReviewFailure(
  message: string,
  accounting: ContributorPatchReviewAccounting,
): never {
  throw new ContributorPatchReviewError(message, accounting);
}

function reviewRequest(
  input: Omit<ContributorPatchPaidPreflight['providerInput'], 'maxOutputTokens'>,
  maxOutputTokens: number,
) {
  return {
    model: input.model,
    temperature: 0,
    stream: false,
    max_tokens: maxOutputTokens,
    response_format: { type: 'json_object' },
    messages: [
      {
        role: 'system',
        content:
          'Independently review a rescue patch. Approve only when it resolves the authorized issue, stays tightly scoped, introduces no security-sensitive behavior, and is maintainable. Return JSON: {approved:boolean, reason:string}.',
      },
      {
        role: 'user',
        content: JSON.stringify({
          issue: input.issue,
          diff: input.diff,
          repositoryChecks: input.repositoryChecks,
        }),
      },
    ],
  };
}

export function validateRepositoryChecks(
  checkCount: number,
  checksPassed: boolean,
): { approved: boolean; reason: string } {
  if (checkCount === 0) {
    return { approved: false, reason: 'at least one deterministic repository check is required' };
  }
  if (!checksPassed) return { approved: false, reason: 'repository checks have not passed' };
  return { approved: true, reason: 'repository checks passed' };
}

export function validateContributorFiles(
  files: Array<{
    filename: string;
    previousFilename?: string;
    status: string;
    patchAvailable: boolean;
  }>,
  changedFiles: number,
): { approved: boolean; reason: string } {
  if (changedFiles === 0) {
    return { approved: false, reason: 'pull request contains no reviewable changes' };
  }
  if (files.length !== changedFiles) {
    return { approved: false, reason: 'GitHub did not return the complete changed-file list' };
  }
  for (const file of files) {
    const paths = [file.filename, file.previousFilename].filter((path): path is string =>
      Boolean(path),
    );
    if (
      paths.some(
        (path) =>
          path.startsWith('/') || path.split('/').includes('..') || forbiddenPath.test(path),
      )
    ) {
      return { approved: false, reason: 'change includes a prohibited path' };
    }
    if (!file.patchAvailable) {
      return { approved: false, reason: 'binary or truncated pull request diffs are unsupported' };
    }
  }
  return { approved: true, reason: 'file policy passed' };
}
