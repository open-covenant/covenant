import { z } from 'zod';
import { PolicyError, requestHash } from './domain.js';
import type { MergeEvidence, MergeReviewArtifact } from './github.js';
import { matchesUsePodModel, readUsePodCompletion } from './usepod-response.js';

const MAX_RESPONSE_BYTES = 65_536;
const MAX_CATALOG_BYTES = 1_048_576;
const MAX_MODELS = 10_000;
const MAX_OUTPUT_TOKENS = 512;
const MAX_REVIEW_DIFF_BYTES = 1_000_000;
const MICROUNITS_PER_USD = 1_000_000n;

export interface IndependentReviewConfig {
  baseUrl: string;
  apiKey: string;
  model: string;
  minimumBalance: string;
  maxInputPriceMicrounits: number;
  maxOutputPriceMicrounits: number;
  maxCostMicrounits: number;
}

export interface IndependentReviewRequest {
  acceptanceHash: string;
  reviewPolicyVersion: 1;
  evidence: MergeEvidence;
  artifact: MergeReviewArtifact;
}

export interface IndependentReviewReceipt {
  approved: true;
  reason: string;
  model: string;
  resolvedModel?: string;
  route: 'marketplace';
  inputHash: string;
  reviewedAt: string;
  providerId?: string;
  requestId?: string;
  costMicrounits?: string;
}

export interface IndependentReviewer {
  health(): Promise<void>;
  review(request: IndependentReviewRequest): Promise<IndependentReviewReceipt>;
}

export class UsePodIndependentReviewer implements IndependentReviewer {
  constructor(
    private readonly config: IndependentReviewConfig,
    private readonly fetcher: typeof fetch = fetch,
    private readonly now: () => Date = () => new Date(),
  ) {}

  async health(): Promise<void> {
    let catalog: Response;
    let balance: Response;
    try {
      [catalog, balance] = await Promise.all([
        this.fetcher(this.url('models'), {
          method: 'GET',
          redirect: 'error',
          headers: { accept: 'application/json' },
          signal: AbortSignal.timeout(15_000),
        }),
        this.fetcher(this.url('balance', false), {
          method: 'GET',
          redirect: 'error',
          headers: { accept: 'application/json' },
          signal: AbortSignal.timeout(15_000),
        }),
      ]);
    } catch {
      throw unavailable('independent review readiness request failed');
    }
    if (!catalog.ok) throw unavailable(`review model catalog returned ${catalog.status}`);
    const catalogBody = z
      .object({
        object: z.literal('list'),
        data: z.array(z.object({ id: z.string().min(1).max(256) }).passthrough()).max(MAX_MODELS),
      })
      .strict()
      .safeParse(await boundedJson(catalog, MAX_CATALOG_BYTES));
    if (!catalogBody.success || !catalogBody.data.data.some(({ id }) => id === this.config.model)) {
      throw unavailable('configured independent review model is unavailable');
    }
    if (!balance.ok) throw unavailable(`review balance endpoint returned ${balance.status}`);
    const balanceBody = z
      .object({ usdc_balance: z.union([z.string(), z.number()]) })
      .passthrough()
      .safeParse(await boundedJson(balance, MAX_RESPONSE_BYTES));
    const bodyValue = balanceBody.success ? String(balanceBody.data.usdc_balance) : '';
    const headerValue = balance.headers.get('x-balance-remaining')?.trim() ?? '';
    if (
      !atomicAtLeast(bodyValue, this.config.minimumBalance) ||
      (headerValue && (!validAtomic(headerValue) || bodyValue !== headerValue))
    ) {
      throw unavailable('independent review balance evidence is invalid or below policy');
    }
  }

  async review(request: IndependentReviewRequest): Promise<IndependentReviewReceipt> {
    const inputHash = requestHash(request);
    if (Buffer.byteLength(request.artifact.diff, 'utf8') > MAX_REVIEW_DIFF_BYTES) {
      throw new PolicyError(
        'independent_review_input_too_large',
        'Independent review diff exceeds the funded review policy',
        422,
      );
    }
    const messages = [
      {
        role: 'system',
        content:
          'Independently authorize a paid maintenance escrow release. Approve only when the patch resolves the accepted issue, stays within the stated file scope, introduces no security-sensitive behavior, and is maintainable. Return JSON: {approved:boolean, reason:string}.',
      },
      {
        role: 'user',
        content: JSON.stringify({
          acceptanceHash: request.acceptanceHash,
          reviewPolicyVersion: request.reviewPolicyVersion,
          repository: request.evidence.repository,
          issueNumber: request.evidence.issueNumber,
          issue: {
            title: request.artifact.issueTitle,
            body: request.artifact.issueBody,
          },
          pullRequestNumber: request.evidence.pullRequestNumber,
          headSha: request.evidence.headCommitOid,
          baseSha: request.evidence.baseCommitOid,
          baseRef: request.evidence.baseRefName,
          mergeCommitSha: request.evidence.mergeCommitOid,
          changedFiles: request.artifact.changedFiles,
          diff: request.artifact.diff,
        }),
      },
    ];
    const maxTokens = fundedOutputTokens(
      {
        model: this.config.model,
        temperature: 0,
        response_format: { type: 'json_object' },
        messages,
      },
      this.config,
    );
    const payload = {
      model: this.config.model,
      temperature: 0,
      max_tokens: maxTokens,
      response_format: { type: 'json_object' },
      messages,
    };
    let response: Response;
    try {
      response = await this.fetcher(this.url('chat/completions'), {
        method: 'POST',
        redirect: 'error',
        headers: {
          'content-type': 'application/json',
          'x-pod-routing-mode': 'marketplace-only',
          'x-pod-no-retention': 'true',
          'x-pod-max-price-input': String(this.config.maxInputPriceMicrounits),
          'x-pod-max-price-output': String(this.config.maxOutputPriceMicrounits),
          'x-request-id': inputHash,
        },
        body: JSON.stringify(payload),
        signal: AbortSignal.timeout(60_000),
      });
    } catch {
      throw unavailable('independent review request failed');
    }
    if (!response.ok) throw unavailable(`independent review returned ${response.status}`);
    const route = response.headers.get('x-pod-route')?.trim().toLowerCase();
    const balance = response.headers.get('x-balance-remaining')?.trim() ?? '';
    if (route !== 'marketplace' || !atomicAtLeast(balance, this.config.minimumBalance)) {
      throw unavailable('independent review route or funded balance evidence is invalid');
    }
    const completion = await readUsePodCompletion(response).catch(() => {
      throw unavailable('independent review returned an invalid completion');
    });
    if (!matchesUsePodModel(this.config.model, completion.model)) {
      throw unavailable('independent review response did not match the configured model');
    }
    let decision: unknown;
    try {
      decision = JSON.parse(completion.content);
    } catch {
      throw unavailable('independent review returned malformed decision JSON');
    }
    const parsed = z
      .object({ approved: z.boolean(), reason: z.string().min(1).max(2_000) })
      .strict()
      .safeParse(decision);
    if (!parsed.success) throw unavailable('independent review returned an invalid decision');
    if (!parsed.data.approved) {
      throw new PolicyError(
        'independent_review_rejected',
        `Independent review rejected the release: ${parsed.data.reason}`,
        422,
      );
    }
    const providerId = optionalHeader(response, 'x-pod-provider-id');
    const requestId = optionalHeader(response, 'x-request-id');
    const costMicrounits = optionalHeader(response, 'x-balance-cost-microunits');
    if (costMicrounits && !boundedAtomic(costMicrounits, this.config.maxCostMicrounits)) {
      throw unavailable('independent review cost evidence exceeds policy');
    }
    return {
      approved: true,
      reason: parsed.data.reason,
      model: this.config.model,
      ...(completion.model === this.config.model ? {} : { resolvedModel: completion.model }),
      route: 'marketplace',
      inputHash,
      reviewedAt: this.now().toISOString(),
      ...(providerId ? { providerId } : {}),
      ...(requestId ? { requestId } : {}),
      ...(costMicrounits ? { costMicrounits } : {}),
    };
  }

  private url(path: string, versioned = true): string {
    const base = new URL(this.config.baseUrl);
    if (base.protocol !== 'https:' || base.username || base.password || base.search || base.hash) {
      throw new Error('Independent review base URL must be a plain HTTPS origin');
    }
    const prefix = base.pathname.replace(/\/+$/, '').replace(/\/v1$/, '');
    if (prefix.includes('/proxy/')) {
      throw new Error('Independent review base URL must not contain a credential path');
    }
    const route = versioned ? `v1/${path}` : path;
    base.pathname = `${prefix}/proxy/${encodeURIComponent(this.config.apiKey)}/${route}`;
    return base.toString();
  }
}

export class MockIndependentReviewer implements IndependentReviewer {
  readonly requests: IndependentReviewRequest[] = [];
  error: Error | null = null;

  async health(): Promise<void> {
    if (this.error) throw this.error;
  }

  async review(request: IndependentReviewRequest): Promise<IndependentReviewReceipt> {
    this.requests.push(structuredClone(request));
    if (this.error) throw this.error;
    return {
      approved: true,
      reason: 'independent review approved the accepted maintenance scope',
      model: 'independent-reviewer',
      route: 'marketplace',
      inputHash: requestHash(request),
      reviewedAt: new Date().toISOString(),
      requestId: requestHash(request),
      costMicrounits: '1',
    };
  }
}

function unavailable(message: string): PolicyError {
  return new PolicyError('independent_review_unavailable', message, 503, true);
}

async function boundedJson(response: Response, limit: number): Promise<unknown> {
  const length = response.headers.get('content-length')?.trim();
  if (length && (!/^\d+$/.test(length) || Number(length) > limit)) {
    throw unavailable('independent review response exceeded its size limit');
  }
  const contentType = response.headers.get('content-type')?.split(';', 1)[0]?.trim().toLowerCase();
  if (contentType !== 'application/json') {
    throw unavailable('independent review response was not JSON');
  }
  if (!response.body) throw unavailable('independent review response was empty');
  const reader = response.body.getReader();
  const chunks: Uint8Array[] = [];
  let received = 0;
  try {
    while (true) {
      const { done, value } = await reader.read();
      if (done) break;
      received += value.byteLength;
      if (received > limit) {
        await reader.cancel();
        throw unavailable('independent review response exceeded its size limit');
      }
      chunks.push(value);
    }
  } finally {
    reader.releaseLock();
  }
  const bytes = new Uint8Array(received);
  let offset = 0;
  for (const chunk of chunks) {
    bytes.set(chunk, offset);
    offset += chunk.byteLength;
  }
  try {
    return JSON.parse(new TextDecoder('utf-8', { fatal: true }).decode(bytes));
  } catch {
    throw unavailable('independent review response was not valid JSON');
  }
}

function optionalHeader(response: Response, name: string): string | undefined {
  const value = response.headers.get(name)?.trim();
  return value || undefined;
}

function atomicAtLeast(value: string, floor: string): boolean {
  return validAtomic(value) && validAtomic(floor) && BigInt(value) >= BigInt(floor);
}

function boundedAtomic(value: string, ceiling: number): boolean {
  return /^\d{1,16}$/.test(value) && BigInt(value) <= BigInt(ceiling);
}

function validAtomic(value: string): boolean {
  return /^\d{1,48}$/.test(value);
}

function fundedOutputTokens(input: unknown, config: IndependentReviewConfig): number {
  const encoded = JSON.stringify(input);
  const inputUpperBound = BigInt(Buffer.byteLength(encoded, 'utf8'));
  const inputCost = divideCeil(
    inputUpperBound * BigInt(config.maxInputPriceMicrounits),
    MICROUNITS_PER_USD,
  );
  const remaining = BigInt(config.maxCostMicrounits) - inputCost;
  if (remaining <= 0n) {
    throw new PolicyError(
      'independent_review_budget_exceeded',
      'Independent review input exceeds its durable cost ceiling',
      422,
    );
  }
  const affordable = (remaining * MICROUNITS_PER_USD) / BigInt(config.maxOutputPriceMicrounits);
  const tokens = Number(
    affordable < BigInt(MAX_OUTPUT_TOKENS) ? affordable : BigInt(MAX_OUTPUT_TOKENS),
  );
  if (!Number.isSafeInteger(tokens) || tokens < 1) {
    throw new PolicyError(
      'independent_review_budget_exceeded',
      'Independent review cannot fund one output token',
      422,
    );
  }
  return tokens;
}

function divideCeil(numerator: bigint, denominator: bigint): bigint {
  return (numerator + denominator - 1n) / denominator;
}
