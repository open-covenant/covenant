import { z } from 'zod';
import type { ContributorPatchReviewer } from './bounties.js';
import type { Config } from './config.js';
import type { RescueBounty } from './domain/index.js';
import { GithubClient, parsePullRequestUrl } from './github.js';
import type { MizukiStore } from './store.js';
import {
  probeUsePod,
  publicUsePodReceipt,
  usePodHeaders,
  usePodReceipt,
  usePodUrl,
} from './usepod.js';

const decisionSchema = z.object({ approved: z.boolean(), reason: z.string().min(1).max(2_000) });
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
    await probeUsePod(this.requestConfig(), this.request);
  }

  async review(bounty: RescueBounty, pullRequestUrl: string) {
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
        approved: false,
        reason: `change exceeds ${job.quote.maxFiles}-file scope`,
        ...evidence,
      };
    }
    const filePolicy = validateContributorFiles(data.files, data.changedFiles);
    if (!filePolicy.approved) return { ...filePolicy, ...evidence };
    const checkPolicy = validateRepositoryChecks(data.checkCount, data.checksPassed);
    if (!checkPolicy.approved) return { ...checkPolicy, ...evidence };
    if (!this.config.usePodApiKey) throw new Error('USEPOD_API_KEY is required for bounty review');
    const requestConfig = this.requestConfig();
    const response = await this.request(usePodUrl(requestConfig, 'chat/completions'), {
      method: 'POST',
      headers: usePodHeaders(requestConfig),
      body: JSON.stringify({
        model: this.config.usePodModel,
        temperature: 0,
        max_tokens: 1_000,
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
              issue: { title: job.quote.issueTitle, body: job.quote.issueBody },
              diff: data.diff,
              repositoryChecks: { count: data.checkCount, passed: data.checksPassed },
            }),
          },
        ],
      }),
      signal: AbortSignal.timeout(60_000),
    });
    if (!response.ok) throw new Error(`UsePod bounty review failed: ${response.status}`);
    const receipt = usePodReceipt(response, this.config.usePodModel);
    const body = z
      .object({
        model: z.string(),
        choices: z.array(z.object({ message: z.object({ content: z.string() }) })).min(1),
      })
      .parse(await response.json());
    if (body.model !== this.config.usePodModel) {
      throw new Error('UsePod bounty review returned a different model');
    }
    return {
      ...decisionSchema.parse(JSON.parse(body.choices[0]!.message.content)),
      ...evidence,
      providerReceipt: publicUsePodReceipt(receipt),
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
    };
  }
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
