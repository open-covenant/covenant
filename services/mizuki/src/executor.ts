import { createHash, randomUUID } from 'node:crypto';
import { z } from 'zod';
import type { Config } from './config.js';
import type { GithubClient } from './github.js';
import type { FinancialPolicy } from './policy-client.js';
import { PendingPolicyOperationError } from './policy-client.js';
import { StateConflictError, type MizukiStore } from './store.js';
import type { Job, ProviderRouteReceipt, Quote, ReviewAttempt, RunArtifacts } from './types.js';
import {
  boundedMaxTokens,
  matchesUsePodModel,
  parseUsePodUsage,
  publicUsePodReceipt,
  usePodHeaders,
  usePodReceipt,
  usePodUrl,
} from './usepod.js';

const safeTokenCount = z
  .number()
  .int()
  .nonnegative()
  .refine(Number.isSafeInteger, 'token count must be a safe integer');
const gatewayRunSchema = z.object({
  status: z.enum(['queued', 'running', 'completed', 'failed', 'cancelled']),
  error: z.string().optional(),
  usage: z.object({ inputTokens: safeTokenCount, outputTokens: safeTokenCount }).optional(),
  costUsd: z.number().nonnegative().finite().optional(),
});
type GatewayRun = z.infer<typeof gatewayRunSchema>;

type Review = {
  approved: boolean;
  reason: string;
  inputTokens: number;
  outputTokens: number;
  providerReceipt: ProviderRouteReceipt;
  costUsd: number;
};

type ReviewPhase = 'implementation' | 'repair';
export type SpendPhase = 'implementation' | 'implementation-review' | 'repair' | 'repair-review';
const MAX_REVIEW_OUTPUT_TOKENS = 512;

const PHASE_WEIGHTS: Record<SpendPhase, number> = {
  implementation: 55,
  'implementation-review': 15,
  repair: 20,
  'repair-review': 10,
};

const gatewayEvidenceSchema = z
  .object({
    ok: z.boolean(),
    checkedAt: z.string().datetime({ offset: true }),
    latencyMs: z.number().int().nonnegative(),
  })
  .strict();
const gatewayReadinessSchema = z
  .object({
    ready: z.boolean(),
    checkedAt: z.string().datetime({ offset: true }),
    ageMs: z.number().int().nonnegative(),
    lastSuccessfulAt: z.string().datetime({ offset: true }).nullable(),
    lastSuccessfulAgeMs: z.number().int().nonnegative().nullable(),
    dependencies: z
      .object({
        model: gatewayEvidenceSchema,
        balance: gatewayEvidenceSchema,
        sandbox: gatewayEvidenceSchema,
        tariff: gatewayEvidenceSchema,
      })
      .strict(),
    failed: z.array(
      z.enum(['model', 'balance', 'sandbox', 'tariff', 'stale', 'ledger', 'runStore']),
    ),
    model: z.string().min(1),
    backend: z.enum(['anthropic', 'openai', 'usepod']),
    provider: z.string().min(1),
    persistentRuns: z.boolean(),
    storage: z
      .object({
        ledger: z.literal(true),
        runStore: z.literal(true),
      })
      .strict(),
  })
  .strict();

export class JobProcessor {
  constructor(
    private readonly config: Config,
    private readonly store: MizukiStore,
    private readonly github: Pick<GithubClient, 'currentHead' | 'publish'>,
    private readonly request: typeof fetch = fetch,
    private readonly onRefunded: (job: Job) => Promise<void> = async () => {},
    private readonly policy?: FinancialPolicy,
  ) {}

  async readiness(): Promise<void> {
    const response = await this.request(`${this.config.codingGatewayUrl}/readyz`, {
      headers: this.config.codingGatewayToken
        ? { authorization: `Bearer ${this.config.codingGatewayToken}` }
        : undefined,
      signal: AbortSignal.timeout(20_000),
    });
    if (!response.ok) throw new Error(`coding gateway is not ready: ${response.status}`);
    const status = gatewayReadinessSchema.parse(await response.json());
    if (
      !status.ready ||
      status.model !== this.config.usePodImplementationModel ||
      status.failed.length > 0 ||
      !status.dependencies.model.ok ||
      !status.dependencies.balance.ok ||
      !status.dependencies.sandbox.ok ||
      !status.dependencies.tariff.ok ||
      !status.storage.ledger ||
      !status.storage.runStore
    ) {
      throw new Error('coding gateway readiness evidence is invalid');
    }
    if (
      this.config.paymentMode === 'live' &&
      (status.backend !== 'usepod' || status.provider !== 'e2b' || status.persistentRuns !== true)
    ) {
      throw new Error('coding gateway does not satisfy the live isolation contract');
    }
  }

  async process(id: string): Promise<void> {
    let job = await this.required(id);
    if (job.state !== 'paid') return;
    try {
      job = await this.store.transitionJob(id, 'paid', 'admitted');
    } catch (cause) {
      if (cause instanceof StateConflictError) return;
      throw cause;
    }
    let inputTokens = 0;
    let outputTokens = 0;
    let variableCostUsd = 0;
    try {
      const head = await this.github.currentHead(
        job.quote.owner,
        job.quote.repo,
        job.quote.defaultBranch,
      );
      if (head !== job.quote.baseSha)
        throw new Error('repository head changed after quote; request a new quote');

      job = await this.store.transitionJob(id, 'admitted', 'running');
      let result = await this.run(
        id,
        'implementation',
        job.quote,
        issuePrompt(job.quote),
        phaseBudgetUsd(job.quote.maxCostUsd, variableCostUsd, 'implementation'),
      );
      inputTokens = result.inputTokens;
      outputTokens = result.outputTokens;
      variableCostUsd = addUsd(variableCostUsd, result.costUsd);
      await this.store.transitionJob(id, 'running', 'validating', {
        runId: result.runId,
        artifacts: result.artifacts,
        inputTokens: result.inputTokens,
        outputTokens: result.outputTokens,
        estimatedCostUsd: variableCostUsd,
      });
      enforcePolicy(job.quote, result.artifacts);
      let review = await this.review(
        id,
        'implementation',
        job.quote,
        result.artifacts,
        phaseBudgetUsd(job.quote.maxCostUsd, variableCostUsd, 'implementation-review'),
      );
      inputTokens += review.inputTokens;
      outputTokens += review.outputTokens;
      variableCostUsd = addUsd(variableCostUsd, review.costUsd);
      await this.recordUsage(id, inputTokens, outputTokens, variableCostUsd);

      if (!review.approved) {
        result = await this.run(
          id,
          'repair',
          job.quote,
          repairPrompt(job.quote, review.reason),
          phaseBudgetUsd(job.quote.maxCostUsd, variableCostUsd, 'repair'),
          result.artifacts.patch,
        );
        enforcePolicy(job.quote, result.artifacts);
        inputTokens += result.inputTokens;
        outputTokens += result.outputTokens;
        variableCostUsd = addUsd(variableCostUsd, result.costUsd);
        await this.recordUsage(id, inputTokens, outputTokens, variableCostUsd);
        review = await this.review(
          id,
          'repair',
          job.quote,
          result.artifacts,
          phaseBudgetUsd(job.quote.maxCostUsd, variableCostUsd, 'repair-review'),
        );
        inputTokens += review.inputTokens;
        outputTokens += review.outputTokens;
        variableCostUsd = addUsd(variableCostUsd, review.costUsd);
        await this.recordUsage(id, inputTokens, outputTokens, variableCostUsd);
        if (!review.approved) {
          await this.store.transitionJob(id, 'validating', 'rejected', {
            error: `review rejected repair: ${review.reason}`,
          });
          throw new Error(`independent review rejected the change: ${review.reason}`);
        }
      }

      const reviewedArtifactHash = artifactHash(job.quote, result.artifacts);
      await this.store.patchJob(id, {
        runId: result.runId,
        artifacts: result.artifacts,
        reviewReceipt: {
          approved: true,
          reason: review.reason,
          reviewedAt: new Date().toISOString(),
          artifactHash: reviewedArtifactHash,
          provider: review.providerReceipt,
        },
      });

      const estimatedCostUsd = variableCostUsd;
      if (estimatedCostUsd > job.quote.maxCostUsd) {
        throw new Error(
          `variable route cost estimate $${estimatedCostUsd.toFixed(4)} exceeds job cap`,
        );
      }
      const finalHead = await this.github.currentHead(
        job.quote.owner,
        job.quote.repo,
        job.quote.defaultBranch,
      );
      if (finalHead !== job.quote.baseSha)
        throw new Error('repository head changed during execution; request a new quote');
      const deliveryJob = await this.required(id);
      if (
        !deliveryJob.artifacts ||
        deliveryJob.reviewReceipt?.artifactHash !==
          artifactHash(deliveryJob.quote, deliveryJob.artifacts)
      ) {
        throw new Error('review receipt does not match the publishable artifacts');
      }
      const prUrl = await this.github.publish(
        deliveryJob,
        deliveryJob.artifacts,
        async (deliveryCommitSha) => {
          await this.bindDelivery(deliveryJob, deliveryJob.artifacts!, deliveryCommitSha);
        },
        async (deliveryEvidence) => {
          await this.store.patchJob(id, { deliveryEvidence });
        },
      );
      const delivered = await this.store.transitionJob(id, 'validating', 'delivered', {
        prUrl,
        artifacts: result.artifacts,
        inputTokens,
        outputTokens,
        estimatedCostUsd,
        error: undefined,
      });
      await this.recordDeliveryReceipts(delivered);
    } catch (cause) {
      if (cause instanceof GatewayRunError || cause instanceof ProviderReviewError) {
        variableCostUsd = addUsd(variableCostUsd, cause.costUsd);
      }
      const error = cause instanceof Error ? cause.message : String(cause);
      const current = await this.required(id);
      if (['delivered', 'refund_pending', 'refunded'].includes(current.state)) return;
      if (current.state !== 'rejected' && current.state !== 'failed') {
        await this.store.transitionJob(id, current.state, 'failed', {
          error,
          inputTokens,
          outputTokens,
          estimatedCostUsd: variableCostUsd,
        });
      } else if (current.state === 'rejected') {
        await this.store.patchJob(id, { error });
      }
      try {
        await this.recordFailureReceipts(await this.required(id));
      } catch (receiptError) {
        console.error(
          `failed to publish failure receipts for job ${id}: ${receiptError instanceof Error ? receiptError.message : String(receiptError)}`,
        );
      }
      await this.refund(id);
    }
  }

  async retryRefund(id: string): Promise<void> {
    const job = await this.required(id);
    if (job.state === 'refunded') return;
    if (job.state !== 'refund_pending') throw new Error('job does not have a pending refund');
    await this.refund(id);
  }

  async reconcileRefunds(): Promise<{ completed: number; pending: number }> {
    let completed = 0;
    let pending = 0;
    for (const job of await this.store.jobsList()) {
      if (job.state !== 'refund_pending') continue;
      await this.refund(job.id);
      if ((await this.required(job.id)).state === 'refunded') completed += 1;
      else pending += 1;
    }
    return { completed, pending };
  }

  async reconcileReceipts(): Promise<void> {
    for (const job of await this.store.jobsList()) {
      try {
        if (job.state === 'delivered') await this.recordDeliveryReceipts(job);
        if (['failed', 'rejected', 'refund_pending', 'refunded'].includes(job.state)) {
          await this.recordFailureReceipts(job);
        }
        if (job.state === 'refunded' && job.refundTransaction) {
          await this.recordRefundReceipts(job);
        }
      } catch (cause) {
        console.error(
          `receipt reconciliation failed for job ${job.id}: ${cause instanceof Error ? cause.message : String(cause)}`,
        );
      }
    }
  }

  async reconcileInFlight(staleAfterMs = 15 * 60_000): Promise<{
    delivered: number;
    refunded: number;
  }> {
    let delivered = 0;
    let refunded = 0;
    const staleBefore = Date.now() - staleAfterMs;
    for (const snapshot of await this.store.jobsList()) {
      if (!['admitted', 'running', 'validating'].includes(snapshot.state)) continue;
      if (Date.parse(snapshot.updatedAt) > staleBefore) continue;
      let job = await this.required(snapshot.id);
      if (job.state === 'validating' && job.artifacts && job.reviewReceipt?.approved) {
        try {
          enforcePolicy(job.quote, job.artifacts);
          if (job.reviewReceipt.artifactHash !== artifactHash(job.quote, job.artifacts)) {
            throw new Error('review receipt does not match the recovery artifacts');
          }
          const head = await this.github.currentHead(
            job.quote.owner,
            job.quote.repo,
            job.quote.defaultBranch,
          );
          if (head !== job.quote.baseSha)
            throw new Error('repository changed before delivery recovery');
          const prUrl = await this.github.publish(
            job,
            job.artifacts,
            async (deliveryCommitSha) => {
              await this.bindDelivery(job, job.artifacts!, deliveryCommitSha);
            },
            async (deliveryEvidence) => {
              await this.store.patchJob(job.id, { deliveryEvidence });
            },
          );
          job = await this.store.transitionJob(job.id, 'validating', 'delivered', { prUrl });
          await this.recordDeliveryReceipts(job);
          delivered += 1;
          continue;
        } catch (cause) {
          const current = await this.required(job.id);
          if (current.state === 'delivered') {
            delivered += 1;
            continue;
          }
          if (current.state !== 'validating') continue;
          job = await this.store.transitionJob(job.id, 'validating', 'failed', {
            error: `delivery recovery failed: ${cause instanceof Error ? cause.message : String(cause)}`,
          });
        }
      } else {
        job = await this.store.transitionJob(job.id, job.state, 'failed', {
          error: 'maintenance run was interrupted before a durable delivery checkpoint',
        });
      }
      try {
        await this.recordFailureReceipts(job);
      } catch {
        // Receipt reconciliation runs independently of the mandatory refund.
      }
      await this.refund(job.id);
      if ((await this.required(job.id)).state === 'refunded') refunded += 1;
    }
    return { delivered, refunded };
  }

  private async run(
    jobId: string,
    phase: 'implementation' | 'repair',
    quote: Quote,
    input: string,
    budgetUsd: number,
    initialPatch?: string,
  ) {
    const runId = await this.submitRun({
      session_id: `${jobId}:${phase}`,
      max_cost_usd: budgetUsd,
      input,
      repository_url: `https://github.com/${quote.owner}/${quote.repo}`,
      base_sha: quote.baseSha,
      validation_commands: quote.validationCommands,
      initial_patch: initialPatch,
    });
    const state = await this.wait(runId, budgetUsd);
    if (state.status !== 'completed') {
      throw new GatewayRunError(
        state.error ?? `coding run ${state.status}`,
        state.costUsd ?? budgetUsd,
      );
    }
    if (!state.usage) {
      throw new GatewayRunError('coding gateway omitted completed token usage', budgetUsd);
    }
    if (state.costUsd === undefined) {
      throw new GatewayRunError('coding gateway omitted completed cost', budgetUsd);
    }
    const costUsd = state.costUsd;
    if (costUsd > budgetUsd) {
      throw new GatewayRunError('coding gateway exceeded its phase spend cap', costUsd);
    }
    const artifactsResponse = await this.request(
      `${this.config.codingGatewayUrl}/v1/runs/${runId}/artifacts`,
      { headers: this.gatewayHeaders(), signal: AbortSignal.timeout(30_000) },
    );
    if (!artifactsResponse.ok) {
      throw new GatewayRunError('coding gateway artifacts unavailable', costUsd);
    }
    return {
      runId,
      artifacts: (await artifactsResponse.json()) as RunArtifacts,
      inputTokens: state.usage.inputTokens,
      outputTokens: state.usage.outputTokens,
      costUsd,
    };
  }

  private async submitRun(body: Record<string, unknown>): Promise<string> {
    let lastError: unknown;
    for (let attempt = 0; attempt < 2; attempt += 1) {
      try {
        const response = await this.request(`${this.config.codingGatewayUrl}/v1/runs`, {
          method: 'POST',
          headers: this.gatewayHeaders({ 'content-type': 'application/json' }),
          body: JSON.stringify(body),
          signal: AbortSignal.timeout(30_000),
        });
        if (!response.ok) {
          const message = `coding gateway rejected job: ${response.status} ${await response.text()}`;
          if (attempt === 0 && response.status >= 500) {
            lastError = new Error(message);
            continue;
          }
          throw new GatewaySubmissionRejectedError(message);
        }
        return z.object({ run_id: z.string().min(1).max(128) }).parse(await response.json()).run_id;
      } catch (cause) {
        if (cause instanceof GatewaySubmissionRejectedError) throw cause;
        lastError = cause;
        if (attempt === 1) break;
      }
    }
    throw lastError instanceof Error ? lastError : new Error(String(lastError));
  }

  private async wait(runId: string, fallbackCostUsd: number): Promise<GatewayRun> {
    const deadline = Date.now() + 12 * 60_000;
    while (Date.now() < deadline) {
      const response = await this.request(`${this.config.codingGatewayUrl}/v1/runs/${runId}`, {
        headers: this.gatewayHeaders(),
        signal: AbortSignal.timeout(30_000),
      });
      if (!response.ok) {
        throw new GatewayRunError(
          `coding gateway status failed: ${response.status}`,
          fallbackCostUsd,
        );
      }
      let run: GatewayRun;
      try {
        run = gatewayRunSchema.parse(await response.json());
      } catch (cause) {
        throw new GatewayRunError(
          `coding gateway returned invalid run evidence: ${cause instanceof Error ? cause.message : String(cause)}`,
          fallbackCostUsd,
        );
      }
      if (!['queued', 'running'].includes(run.status)) return run;
      await delay(1_000);
    }
    await this.request(`${this.config.codingGatewayUrl}/v1/runs/${runId}/stop`, {
      method: 'POST',
      headers: this.gatewayHeaders(),
      signal: AbortSignal.timeout(30_000),
    });
    throw new GatewayRunError('coding run timed out', fallbackCostUsd);
  }

  private async review(
    jobId: string,
    phase: ReviewPhase,
    quote: Quote,
    artifacts: RunArtifacts,
    budgetUsd: number,
  ): Promise<Review> {
    if (!this.config.usePodApiKey)
      throw new Error('USEPOD_API_KEY is required for independent review');
    const requestConfig = {
      baseUrl: this.config.usePodBaseUrl,
      token: this.config.usePodApiKey,
      model: this.config.usePodModel,
      maxInputPriceMicrounits: this.config.usePodMaxInputPriceMicrounits,
      maxOutputPriceMicrounits: this.config.usePodMaxOutputPriceMicrounits,
      minimumBalance: this.config.usePodMinimumBalance,
      maxCostMicrounits: Math.floor(budgetUsd * 1_000_000),
    };
    const draft = {
      model: this.config.usePodModel,
      temperature: 0,
      max_tokens: MAX_REVIEW_OUTPUT_TOKENS,
      response_format: { type: 'json_object' },
      messages: [
        {
          role: 'system',
          content:
            'Independently review a small maintenance patch. Approve only if it resolves the issue, is scoped, safe, and validated. Return JSON: {approved:boolean, reason:string}.',
        },
        {
          role: 'user',
          content: JSON.stringify({
            issue: { title: quote.issueTitle, body: quote.issueBody },
            patch: artifacts.patch,
            files: artifacts.files,
            validations: artifacts.validations,
          }),
        },
      ],
    };
    const maxTokens = boundedMaxTokens(
      draft,
      requestConfig.maxCostMicrounits,
      requestConfig.maxInputPriceMicrounits,
      requestConfig.maxOutputPriceMicrounits,
      MAX_REVIEW_OUTPUT_TOKENS,
    );
    const attempt: ReviewAttempt = {
      id: randomUUID(),
      phase,
      artifactHash: artifactHash(quote, artifacts),
      status: 'pending',
      costUsd: budgetUsd,
      reviewedAt: new Date().toISOString(),
    };
    await this.reserveReviewAttempt(jobId, attempt);
    let response: Response;
    try {
      response = await this.request(usePodUrl(requestConfig, 'chat/completions'), {
        method: 'POST',
        headers: usePodHeaders(requestConfig),
        body: JSON.stringify({ ...draft, max_tokens: maxTokens }),
        signal: AbortSignal.timeout(60_000),
      });
    } catch (cause) {
      const message = `UsePod reviewer request failed: ${cause instanceof Error ? cause.message : String(cause)}`;
      await this.markReviewAttemptFailed(jobId, attempt.id, message);
      throw new ProviderReviewError(message, budgetUsd);
    }

    let receipt;
    try {
      receipt = usePodReceipt(response, this.config.usePodModel, requestConfig.minimumBalance);
    } catch (cause) {
      const message = cause instanceof Error ? cause.message : String(cause);
      await this.markReviewAttemptFailed(jobId, attempt.id, message);
      throw new ProviderReviewError(message, budgetUsd);
    }
    const provider = publicUsePodReceipt(receipt);
    const costUsd = provider.costMicrounits
      ? Number(BigInt(provider.costMicrounits)) / 1_000_000
      : budgetUsd;
    try {
      await this.updateReviewAttempt(jobId, attempt.id, {
        status: 'received',
        provider,
        costUsd,
      });
    } catch (cause) {
      throw new ProviderReviewError(
        `review receipt persistence failed: ${cause instanceof Error ? cause.message : String(cause)}`,
        budgetUsd,
      );
    }
    try {
      if (costUsd > budgetUsd) {
        throw new Error('UsePod reviewer exceeded its phase spend cap');
      }
      if (!response.ok) {
        throw new Error(`UsePod reviewer failed: ${response.status} ${await response.text()}`);
      }
      const body = z
        .object({
          model: z.string(),
          choices: z.array(z.object({ message: z.object({ content: z.string().min(1) }) })).min(1),
          usage: z.unknown(),
        })
        .parse(await response.json());
      if (!matchesUsePodModel(this.config.usePodModel, body.model)) {
        throw new Error('UsePod reviewer returned a different model');
      }
      const usage = parseUsePodUsage(body.usage);
      const decision = z
        .object({ approved: z.boolean(), reason: z.string().min(1).max(2_000) })
        .strict()
        .parse(JSON.parse(body.choices[0]!.message.content));
      await this.updateReviewAttempt(jobId, attempt.id, {
        status: 'completed',
        inputTokens: usage.promptTokens,
        outputTokens: usage.completionTokens,
        approved: decision.approved,
        reason: decision.reason,
      });
      return {
        approved: decision.approved,
        reason: decision.reason,
        inputTokens: usage.promptTokens,
        outputTokens: usage.completionTokens,
        providerReceipt: provider,
        costUsd,
      };
    } catch (cause) {
      const message = cause instanceof Error ? cause.message : String(cause);
      await this.markReviewAttemptFailed(jobId, attempt.id, message);
      throw new ProviderReviewError(message, costUsd);
    }
  }

  private async refund(id: string): Promise<void> {
    let job = await this.required(id);
    try {
      if (job.state !== 'refund_pending') {
        job = await this.store.transitionJob(id, ['failed', 'rejected'], 'refund_pending');
        await this.store.appendLedger({
          kind: 'refund_liability',
          referenceId: id,
          asset: USDC_MAINNET,
          amountAtomic: job.payment.amountAtomic,
          amountUsd: Number(job.payment.amountAtomic) / 1_000_000,
          transaction: job.payment.transaction,
        });
        await this.store.appendActivity('refund.pending', id, {
          settlementTransaction: job.payment.transaction,
        });
      }
      if (this.config.paymentMode === 'mock') {
        const refunded = await this.completeRefund(job, `mock-refund-${job.id}`);
        await this.afterRefund(refunded);
        return;
      }
      if (!this.policy) throw new Error('policy signer is not configured');
      const operation = await this.policy.refund(job.id, job.payment.transaction);
      if (
        operation.kind !== 'refund' ||
        operation.status !== 'finalized' ||
        operation.asset !== USDC_MAINNET ||
        operation.recipient !== job.payment.payer ||
        operation.amountUsdCents !== Number(job.payment.amountAtomic) / 10_000
      ) {
        throw new Error('policy signer returned refund evidence that does not match the payment');
      }
      if (!operation.transactionSignature) throw new Error('refund operation has no transaction');
      await this.store.patchJob(id, { refundOperationId: operation.id });
      const refunded = await this.completeRefund(job, operation.transactionSignature);
      await this.afterRefund(refunded);
    } catch (cause) {
      const current = await this.required(id);
      if (current.state === 'refunded') {
        await this.afterRefund(current);
        return;
      }
      if (cause instanceof StateConflictError) return;
      const refundError = cause instanceof Error ? cause.message : String(cause);
      await this.store.patchJob(id, {
        error: `${job.error ?? 'job failed'}; refund pending: ${refundError}`,
        ...(cause instanceof PendingPolicyOperationError
          ? { refundOperationId: cause.operationId }
          : {}),
      });
    }
  }

  private async bindDelivery(
    job: Job,
    artifacts: RunArtifacts,
    deliveryCommitSha: string,
  ): Promise<void> {
    await this.store.patchJob(job.id, { deliveryCommitSha });
    if (this.config.paymentMode !== 'live') return;
    if (!this.policy || !job.refundLiabilityId) {
      throw new Error('registered refund liability is required before delivery publication');
    }
    const input = {
      jobId: job.id,
      settlementSignature: job.payment.transaction,
      reviewedHeadSha: deliveryCommitSha,
      reviewedBaseSha: job.quote.baseSha,
      reviewedBaseRef: job.quote.defaultBranch,
      reviewedDiffHash: createHash('sha256').update(artifacts.patch).digest('hex'),
    };
    const liability = await this.policy.bindRefundLiabilityDelivery(job.refundLiabilityId, input);
    if (
      liability.id !== job.refundLiabilityId ||
      liability.jobId !== job.id ||
      liability.settlementSignature !== job.payment.transaction ||
      liability.reviewedHeadSha !== input.reviewedHeadSha ||
      liability.reviewedBaseSha !== input.reviewedBaseSha ||
      liability.reviewedBaseRef !== input.reviewedBaseRef ||
      liability.reviewedDiffHash !== input.reviewedDiffHash ||
      !liability.deliveryBoundAt ||
      !liability.deliveryBindingHash
    ) {
      throw new Error('policy signer returned a mismatched delivery binding');
    }
  }

  private async completeRefund(job: Job, transaction: string): Promise<Job> {
    const refunded = await this.store.transitionJob(job.id, 'refund_pending', 'refunded', {
      refundTransaction: transaction,
    });
    await this.recordRefundReceipts(refunded);
    return refunded;
  }

  private async recordDeliveryReceipts(job: Job): Promise<void> {
    await this.store.appendLedger({
      kind: 'route_cost',
      referenceId: job.id,
      asset: 'USD',
      amountAtomic: '0',
      amountUsd: job.estimatedCostUsd,
    });
    const exists = (await this.store.activity(500)).some(
      (event) => event.kind === 'job.delivered' && event.subjectId === job.id,
    );
    if (!exists) {
      await this.store.appendActivity('job.delivered', job.id, {
        prUrl: job.prUrl,
        estimatedCostUsd: job.estimatedCostUsd,
      });
    }
  }

  private async recordFailureReceipts(job: Job): Promise<void> {
    if (job.estimatedCostUsd > 0) {
      await this.store.appendLedger({
        kind: 'route_cost',
        referenceId: job.id,
        asset: 'USD',
        amountAtomic: '0',
        amountUsd: job.estimatedCostUsd,
      });
    }
    const exists = (await this.store.activity(500)).some(
      (event) => event.kind === 'job.failed' && event.subjectId === job.id,
    );
    if (!exists) await this.store.appendActivity('job.failed', job.id, {});
  }

  private async recordRefundReceipts(job: Job): Promise<void> {
    if (!job.refundTransaction) return;
    await this.store.appendLedger({
      kind: 'refund_completed',
      referenceId: job.id,
      asset: USDC_MAINNET,
      amountAtomic: job.payment.amountAtomic,
      amountUsd: Number(job.payment.amountAtomic) / 1_000_000,
      transaction: job.refundTransaction,
    });
    const exists = (await this.store.activity(500)).some(
      (event) => event.kind === 'refund.completed' && event.subjectId === job.id,
    );
    if (!exists) {
      await this.store.appendActivity('refund.completed', job.id, {
        transaction: job.refundTransaction,
      });
    }
  }

  private async afterRefund(job: Job): Promise<void> {
    try {
      await this.onRefunded(job);
    } catch (cause) {
      await this.store.appendActivity('bounty.creation_failed', job.id, {
        status: 'creation_failed',
        error: cause instanceof Error ? cause.message : String(cause),
      });
    }
  }

  private async required(id: string): Promise<Job> {
    const job = await this.store.job(id);
    if (!job) throw new Error(`unknown job: ${id}`);
    return job;
  }

  private gatewayHeaders(headers: Record<string, string> = {}): Record<string, string> {
    return {
      ...headers,
      ...(this.config.codingGatewayToken
        ? { authorization: `Bearer ${this.config.codingGatewayToken}` }
        : {}),
    };
  }

  private async recordUsage(
    id: string,
    inputTokens: number,
    outputTokens: number,
    variableCostUsd: number,
  ): Promise<void> {
    await this.store.patchJob(id, {
      inputTokens,
      outputTokens,
      estimatedCostUsd: variableCostUsd,
    });
  }

  private async reserveReviewAttempt(id: string, attempt: ReviewAttempt): Promise<void> {
    const job = await this.required(id);
    const estimatedCostUsd = addUsd(job.estimatedCostUsd, attempt.costUsd);
    if (!Number.isFinite(estimatedCostUsd) || estimatedCostUsd > job.quote.maxCostUsd + 1e-9) {
      throw new Error('review reservation exceeds the job spend cap');
    }
    await this.store.patchJob(id, {
      reviewAttempts: [...(job.reviewAttempts ?? []), attempt],
      estimatedCostUsd,
    });
  }

  private async updateReviewAttempt(
    id: string,
    attemptId: string,
    patch: Partial<Omit<ReviewAttempt, 'id'>>,
  ): Promise<void> {
    const job = await this.required(id);
    const attempts = job.reviewAttempts ?? [];
    if (!attempts.some((attempt) => attempt.id === attemptId)) {
      throw new Error('review receipt checkpoint is missing');
    }
    const previous = attempts.find((attempt) => attempt.id === attemptId)!;
    const nextCostUsd = patch.costUsd ?? previous.costUsd;
    const estimatedCostUsd = roundUsd(job.estimatedCostUsd - previous.costUsd + nextCostUsd);
    if (!Number.isFinite(estimatedCostUsd) || estimatedCostUsd < 0) {
      throw new Error('review cost reconciliation is invalid');
    }
    await this.store.patchJob(id, {
      reviewAttempts: attempts.map((attempt) =>
        attempt.id === attemptId ? { ...attempt, ...patch } : attempt,
      ),
      estimatedCostUsd,
    });
  }

  private async markReviewAttemptFailed(
    id: string,
    attemptId: string,
    error: string,
  ): Promise<void> {
    try {
      await this.updateReviewAttempt(id, attemptId, { status: 'failed', error });
    } catch {
      // The pending full-cost checkpoint remains the conservative source of truth.
    }
  }
}

class GatewayRunError extends Error {
  constructor(
    message: string,
    readonly costUsd: number,
  ) {
    super(message);
  }
}

class GatewaySubmissionRejectedError extends Error {}

class ProviderReviewError extends Error {
  constructor(
    message: string,
    readonly costUsd: number,
  ) {
    super(message);
  }
}

const USDC_MAINNET = 'EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v';
const FORBIDDEN_PATH =
  /(^|\/)(\.github\/workflows|\.env|secrets?|vendor|generated|dist|build|node_modules)(\/|$)|(^|\/)(package-lock\.json|pnpm-lock\.yaml|yarn\.lock)$/i;

export function enforcePolicy(quote: Quote, artifacts: RunArtifacts): void {
  if (!artifacts.patch.trim() || artifacts.changedFiles.length === 0)
    throw new Error('coding run produced no change');
  if (artifacts.changedFiles.length > quote.maxFiles) {
    throw new Error(`change exceeds ${quote.maxFiles}-file ${quote.class} scope`);
  }
  const duplicate = new Set(artifacts.changedFiles);
  if (duplicate.size !== artifacts.changedFiles.length)
    throw new Error('artifact list contains duplicate paths');
  const filePaths = artifacts.files.map((file) => file.path);
  if (new Set(filePaths).size !== filePaths.length) {
    throw new Error('artifact files contain duplicate paths');
  }
  if (
    filePaths.length !== artifacts.changedFiles.length ||
    filePaths.some((path) => !duplicate.has(path))
  ) {
    throw new Error('publishable files do not exactly match the reviewed change list');
  }
  if (artifacts.patch.length > 1_000_000) throw new Error('artifact patch exceeds review limit');
  let contentBytes = 0;
  for (const file of artifacts.files) {
    const bytes = Buffer.byteLength(file.content, 'utf8');
    if (bytes > 128_000) throw new Error(`publishable file exceeds review limit: ${file.path}`);
    contentBytes += bytes;
  }
  if (contentBytes > 512_000) throw new Error('publishable files exceed review limit');
  for (const path of artifacts.changedFiles) {
    if (path.startsWith('/') || path.includes('..') || FORBIDDEN_PATH.test(path)) {
      throw new Error(`forbidden path in change: ${path}`);
    }
    if (!artifacts.files.some((file) => file.path === path)) {
      throw new Error(`deleted, binary, or oversized files are unsupported: ${path}`);
    }
  }
  if (quote.validationCommands.length > 0) {
    if (artifacts.validations.length !== quote.validationCommands.length) {
      throw new Error('declared validations did not all run');
    }
    if (artifacts.validations.some((validation) => validation.exitCode !== 0)) {
      throw new Error('declared validation failed');
    }
  }
}

export function artifactHash(quote: Quote, artifacts: RunArtifacts): string {
  return createHash('sha256')
    .update(canonicalJson({ baseSha: quote.baseSha, artifacts }))
    .digest('hex');
}

function canonicalJson(value: unknown): string {
  if (value === null || typeof value !== 'object') return JSON.stringify(value);
  if (Array.isArray(value)) return `[${value.map(canonicalJson).join(',')}]`;
  return `{${Object.entries(value as Record<string, unknown>)
    .sort(([left], [right]) => left.localeCompare(right))
    .map(([key, entry]) => `${JSON.stringify(key)}:${canonicalJson(entry)}`)
    .join(',')}}`;
}

export function phaseBudgetPlan(maxCostUsd: number): Record<SpendPhase, number> {
  const total = usdMicrounits(maxCostUsd, 'job spend cap');
  return Object.fromEntries(
    (Object.entries(PHASE_WEIGHTS) as Array<[SpendPhase, number]>).map(([phase, weight]) => [
      phase,
      Number((BigInt(total) * BigInt(weight)) / 100n) / 1_000_000,
    ]),
  ) as Record<SpendPhase, number>;
}

export function phaseBudgetUsd(maxCostUsd: number, spentUsd: number, phase: SpendPhase): number {
  const planned = usdMicrounits(phaseBudgetPlan(maxCostUsd)[phase], `${phase} budget`);
  if (!Number.isFinite(spentUsd) || spentUsd < 0) throw new Error('job spend is invalid');
  const remaining = Math.max(0, Math.floor((maxCostUsd - spentUsd + 1e-12) * 1_000_000));
  const granted = Math.min(planned, remaining);
  if (granted <= 0) throw new Error(`job spend cap exhausted before ${phase}`);
  return granted / 1_000_000;
}

function addUsd(left: number, right: number): number {
  return roundUsd(roundUsd(left) + roundUsd(right));
}

function roundUsd(value: number): number {
  if (!Number.isFinite(value) || value < 0) throw new Error('job spend is invalid');
  const microunits = Math.ceil(value * 1_000_000 - 1e-9);
  if (!Number.isSafeInteger(microunits)) throw new Error('job spend exceeds the accounting range');
  return microunits / 1_000_000;
}

function usdMicrounits(value: number, name: string): number {
  const microunits = Math.floor(value * 1_000_000 + 1e-9);
  if (!Number.isSafeInteger(microunits) || microunits <= 0) {
    throw new Error(`${name} must be a positive finite amount`);
  }
  return microunits;
}

function issuePrompt(quote: Quote): string {
  return `Resolve GitHub issue #${quote.issueNumber}: ${quote.issueTitle}\n\n${quote.issueBody}\n\nKeep the change within ${quote.maxFiles} files. Do not broaden scope. Run these required checks before finishing:\n${quote.validationCommands.map((command) => `- ${command}`).join('\n')}`;
}

function repairPrompt(quote: Quote, reason: string): string {
  return `Repair the existing patch for issue #${quote.issueNumber}. Independent review found: ${reason}\nDo not broaden the original issue scope. Run these required checks before finishing:\n${quote.validationCommands.map((command) => `- ${command}`).join('\n')}`;
}

function delay(ms: number): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, ms));
}
