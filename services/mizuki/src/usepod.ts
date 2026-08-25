import { z } from 'zod';
import type { ProviderRouteReceipt } from './types.js';

export interface UsePodRequestConfig {
  baseUrl: string;
  token: string;
  model: string;
  maxInputPriceMicrounits: number;
  maxOutputPriceMicrounits: number;
  minimumBalance?: string;
}

export interface UsePodRouteReceipt {
  model: string;
  resolvedModel?: string;
  route: 'marketplace';
  balanceRemaining: string;
  providerId?: string;
  requestId?: string;
  costMicrounits?: string;
}

export interface UsePodUsage {
  promptTokens: number;
  completionTokens: number;
}

export type UsePodReviewDecision = { approved: boolean; reason: string };

export class UsePodReceiptError extends Error {
  constructor(
    message: string,
    readonly retryable: boolean,
  ) {
    super(message);
    this.name = 'UsePodReceiptError';
  }
}

export type UsePodChatCompletion =
  | { ok: true; model: string; content: string; usage: unknown }
  | { ok: false; error: string; retryable: boolean; model?: string; usage?: unknown };

const MICROUNITS_PER_USD = 1_000_000n;
const MAX_RECEIPT_MICROUNITS = BigInt(Number.MAX_SAFE_INTEGER);
const MAX_CATALOG_BYTES = 1_048_576;
const MAX_CATALOG_MODELS = 10_000;
const MAX_REVIEW_RESPONSE_BYTES = 64 * 1024;
const MAX_STREAM_FRAMES = 1_024;
const RESOLVED_MODELS: ReadonlyMap<string, ReadonlySet<string>> = new Map([
  [
    'deepseek-v4-flash',
    new Set([
      'deepseek-v4-flash-0731',
      'deepseek-v4-flash-260425',
      'deepseek/deepseek-v4-flash-0731',
    ]),
  ],
]);
const modelIdSchema = z
  .string()
  .min(1)
  .max(256)
  .regex(/^[A-Za-z0-9][A-Za-z0-9._:/-]*$/);
const requestIdSchema = z
  .string()
  .min(1)
  .max(128)
  .regex(/^[A-Za-z0-9][A-Za-z0-9._:/-]*$/);
const messageChoiceSchema = z
  .object({
    index: z.literal(0),
    finish_reason: z.literal('stop'),
    message: z
      .object({
        role: z.literal('assistant').optional(),
        content: z.string(),
        reasoning_content: z.string().nullable().optional(),
      })
      .strict(),
  })
  .passthrough();
const completionSchema = z
  .object({
    id: requestIdSchema.optional(),
    model: modelIdSchema,
    choices: z.array(messageChoiceSchema).length(1),
    usage: z.unknown().optional(),
  })
  .passthrough()
  .refine((value) => !Object.hasOwn(value, 'error'));
const deltaChoiceSchema = z
  .object({
    index: z.literal(0),
    finish_reason: z.union([z.literal('stop'), z.null()]).optional(),
    delta: z
      .object({
        role: z.literal('assistant').optional(),
        content: z.string().nullable().optional(),
        reasoning_content: z.string().nullable().optional(),
      })
      .strict(),
  })
  .passthrough();
const streamChunkSchema = z
  .object({
    id: requestIdSchema.optional(),
    model: modelIdSchema,
    choices: z.array(deltaChoiceSchema).max(1),
    usage: z.unknown().optional(),
  })
  .passthrough()
  .refine((value) => !Object.hasOwn(value, 'error'));
const reviewDecisionSchema = z
  .object({ approved: z.boolean(), reason: z.string().min(1).max(2_000) })
  .strict();
const modelCatalogSchema = z
  .object({
    object: z.literal('list'),
    data: z
      .array(z.object({ id: z.string().min(1).max(256) }).passthrough())
      .min(1)
      .max(MAX_CATALOG_MODELS),
  })
  .strict();

export function usePodUrl(config: UsePodRequestConfig, path: string): string {
  const base = new URL(config.baseUrl);
  if (base.protocol !== 'https:' || base.username || base.password || base.search || base.hash) {
    throw new Error('USEPOD_BASE_URL must be HTTPS and contain no credentials, query, or fragment');
  }
  const prefix = base.pathname.replace(/\/+$/, '').replace(/\/v1$/, '');
  if (prefix.includes('/proxy/')) {
    throw new Error('USEPOD_BASE_URL must not contain a tokenized proxy path');
  }
  base.pathname = `${prefix}/proxy/${encodeURIComponent(config.token)}/v1/${path.replace(/^\/+/, '')}`;
  return base.toString();
}

export function usePodHeaders(config: UsePodRequestConfig): Record<string, string> {
  return {
    'content-type': 'application/json',
    'x-pod-routing-mode': 'marketplace-only',
    'x-pod-no-retention': 'true',
    'x-pod-max-price-input': String(config.maxInputPriceMicrounits),
    'x-pod-max-price-output': String(config.maxOutputPriceMicrounits),
  };
}

export function usePodReceipt(
  response: Response,
  model: string,
  minimumBalance = process.env.USEPOD_MIN_BALANCE ?? '1',
  resolvedModel?: string,
): UsePodRouteReceipt {
  if (!validModelId(model) || (resolvedModel !== undefined && !validModelId(resolvedModel))) {
    throw new UsePodReceiptError('UsePod returned an invalid model identity', false);
  }
  const route = response.headers.get('x-pod-route')?.trim().toLowerCase();
  if (!route) {
    throw new UsePodReceiptError('UsePod did not prove a marketplace route', true);
  }
  if (route !== 'marketplace') {
    throw new UsePodReceiptError('UsePod returned an unacceptable route', false);
  }
  const balanceRemaining = response.headers.get('x-balance-remaining')?.trim();
  if (!balanceRemaining) {
    throw new UsePodReceiptError('UsePod did not prove a funded balance after the request', true);
  }
  if (!validDecimal(balanceRemaining)) {
    throw new UsePodReceiptError('UsePod returned an invalid balance receipt', false);
  }
  if (!decimalAtLeast(balanceRemaining, minimumBalance)) {
    throw new UsePodReceiptError(
      'UsePod balance is below the configured funded-balance floor',
      false,
    );
  }

  const providerId = optionalReceiptId(response, 'x-pod-provider-id');
  const requestId = optionalReceiptId(response, 'x-request-id');
  const costMicrounits = optionalHeader(response, 'x-balance-cost-microunits');
  if (costMicrounits && !validCostMicrounits(costMicrounits)) {
    throw new UsePodReceiptError('UsePod returned an invalid cost receipt', false);
  }
  return {
    model,
    ...(resolvedModel && resolvedModel !== model ? { resolvedModel } : {}),
    route,
    balanceRemaining,
    ...(providerId ? { providerId } : {}),
    ...(requestId ? { requestId } : {}),
    ...(costMicrounits ? { costMicrounits } : {}),
  };
}

export function boundedMaxTokens(
  payload: unknown,
  budgetMicrounits: number,
  inputPriceMicrounits: number,
  outputPriceMicrounits: number,
  ceiling: number,
): number {
  for (const [name, value] of [
    ['budget', budgetMicrounits],
    ['input price', inputPriceMicrounits],
    ['output price', outputPriceMicrounits],
    ['token ceiling', ceiling],
  ] as const) {
    if (!Number.isSafeInteger(value) || value <= 0) {
      throw new Error(`UsePod ${name} must be a positive safe integer`);
    }
  }
  const encoded = JSON.stringify(payload);
  if (encoded === undefined) throw new Error('UsePod request payload is not serializable');
  const inputTokenUpperBound = BigInt(Math.max(1, Buffer.byteLength(encoded, 'utf8')));
  const inputCost = divideCeil(
    inputTokenUpperBound * BigInt(inputPriceMicrounits),
    MICROUNITS_PER_USD,
  );
  const remaining = BigInt(budgetMicrounits) - inputCost;
  if (remaining <= 0n) throw new Error('UsePod request exceeds its provider spend budget');
  const affordable = (remaining * MICROUNITS_PER_USD) / BigInt(outputPriceMicrounits);
  const tokens = Number(affordable < BigInt(ceiling) ? affordable : BigInt(ceiling));
  if (!Number.isSafeInteger(tokens) || tokens < 1) {
    throw new Error('UsePod request cannot fund one output token');
  }
  return tokens;
}

export function parseUsePodUsage(value: unknown): UsePodUsage {
  if (!isRecord(value)) throw new Error('UsePod returned invalid token usage');
  const promptTokens = value.prompt_tokens;
  const completionTokens = value.completion_tokens;
  if (!safeTokenCount(promptTokens) || !safeTokenCount(completionTokens)) {
    throw new Error('UsePod returned invalid token usage');
  }
  return { promptTokens, completionTokens };
}

export function parseUsePodReviewDecision(content: string): UsePodReviewDecision {
  try {
    const keys = topLevelObjectKeys(content);
    if (new Set(keys).size !== keys.length) throw new Error('duplicate decision key');
    return reviewDecisionSchema.parse(JSON.parse(content) as unknown);
  } catch {
    throw new Error('UsePod reviewer returned an invalid decision');
  }
}

export function matchesUsePodModel(requested: string, returned: unknown): boolean {
  if (!validModelId(requested) || !validModelId(returned)) return false;
  if (returned === requested) return true;
  return RESOLVED_MODELS.get(requested)?.has(returned) ?? false;
}

export function publicUsePodReceipt(receipt: UsePodRouteReceipt): ProviderRouteReceipt {
  return {
    model: receipt.model,
    ...(receipt.resolvedModel ? { resolvedModel: receipt.resolvedModel } : {}),
    route: receipt.route,
    ...(receipt.providerId ? { providerId: receipt.providerId } : {}),
    ...(receipt.requestId ? { requestId: receipt.requestId } : {}),
    ...(receipt.costMicrounits ? { costMicrounits: receipt.costMicrounits } : {}),
  };
}

export async function readUsePodChatCompletion(
  response: Response,
  maxBytes = MAX_REVIEW_RESPONSE_BYTES,
): Promise<UsePodChatCompletion> {
  if (!Number.isSafeInteger(maxBytes) || maxBytes < 1 || maxBytes > MAX_REVIEW_RESPONSE_BYTES) {
    throw new Error('UsePod review response limit is invalid');
  }

  let text: string;
  try {
    text = await boundedText(response, maxBytes);
  } catch (cause) {
    return completionFailure(
      cause instanceof ResponseLimitError
        ? 'UsePod review response exceeded the size limit'
        : 'UsePod review response could not be read',
      true,
    );
  }

  const body = normalizeResponseStart(text);
  if (body.startsWith('{') || body.startsWith('[')) return parseJsonCompletion(body);
  if (body.startsWith('data:') || body.startsWith(':')) return parseEventStream(body);
  return completionFailure('UsePod review returned an unsupported response format', true);
}

export async function probeUsePodCatalog(
  config: UsePodRequestConfig,
  request: typeof fetch = fetch,
): Promise<void> {
  let response: Response;
  try {
    response = await request(usePodUrl(config, 'models'), {
      method: 'GET',
      redirect: 'error',
      headers: { accept: 'application/json' },
      signal: AbortSignal.timeout(15_000),
    });
  } catch {
    throw new Error('UsePod model catalog request failed');
  }
  if (!response.ok) {
    throw new Error(`UsePod model catalog failed with HTTP ${response.status}`);
  }
  const contentType = response.headers.get('content-type')?.split(';', 1)[0]?.trim().toLowerCase();
  if (contentType !== 'application/json') {
    throw new Error('UsePod model catalog returned a non-JSON response');
  }

  const catalog = modelCatalogSchema.safeParse(await boundedJson(response));
  if (!catalog.success) {
    throw new Error('UsePod model catalog returned malformed JSON');
  }
  if (!catalog.data.data.some(({ id }) => id === config.model)) {
    throw new Error('UsePod model catalog does not include the configured model');
  }
}

async function boundedJson(response: Response): Promise<unknown> {
  const contentLength = response.headers.get('content-length')?.trim();
  if (contentLength) {
    if (!/^\d+$/.test(contentLength) || BigInt(contentLength) > BigInt(MAX_CATALOG_BYTES)) {
      throw new Error('UsePod model catalog exceeded the response size limit');
    }
  }
  if (!response.body) throw new Error('UsePod model catalog returned malformed JSON');

  const reader = response.body.getReader();
  const decoder = new TextDecoder('utf-8', { fatal: true });
  const chunks: string[] = [];
  let received = 0;
  try {
    while (true) {
      const { done, value } = await reader.read();
      if (done) break;
      received += value.byteLength;
      if (received > MAX_CATALOG_BYTES) {
        await reader.cancel();
        throw new Error('UsePod model catalog exceeded the response size limit');
      }
      chunks.push(decoder.decode(value, { stream: true }));
    }
    chunks.push(decoder.decode());
  } finally {
    reader.releaseLock();
  }

  try {
    return JSON.parse(chunks.join('')) as unknown;
  } catch {
    throw new Error('UsePod model catalog returned malformed JSON');
  }
}

async function boundedText(response: Response, maxBytes: number): Promise<string> {
  const contentLength = response.headers.get('content-length')?.trim();
  if (contentLength) {
    if (!/^\d+$/.test(contentLength) || BigInt(contentLength) > BigInt(maxBytes)) {
      throw new ResponseLimitError();
    }
  }
  if (!response.body) throw new Error('missing response body');

  const reader = response.body.getReader();
  const decoder = new TextDecoder('utf-8', { fatal: true });
  const chunks: string[] = [];
  let received = 0;
  try {
    while (true) {
      const { done, value } = await reader.read();
      if (done) break;
      received += value.byteLength;
      if (received > maxBytes) {
        await reader.cancel();
        throw new ResponseLimitError();
      }
      chunks.push(decoder.decode(value, { stream: true }));
    }
    chunks.push(decoder.decode());
    return chunks.join('');
  } finally {
    reader.releaseLock();
  }
}

function parseJsonCompletion(text: string): UsePodChatCompletion {
  let value: unknown;
  try {
    value = JSON.parse(text) as unknown;
  } catch {
    return completionFailure('UsePod review returned malformed JSON', true);
  }

  const evidence = responseEvidence(value);
  if (!isRecord(value) || !validModelId(value.model)) {
    return completionFailure('UsePod review returned an invalid model identity', false, evidence);
  }
  if (Object.hasOwn(value, 'id') && !requestIdSchema.safeParse(value.id).success) {
    return completionFailure('UsePod review returned an invalid request identity', false, evidence);
  }
  const parsed = completionSchema.safeParse(value);
  if (!parsed.success) {
    return completionFailure('UsePod review returned a malformed completion', true, evidence);
  }
  const choice = parsed.data.choices[0];
  if (!choice) {
    return completionFailure('UsePod review returned an incomplete completion', true, evidence);
  }
  if (parsed.data.usage === undefined) {
    return completionFailure('UsePod review omitted token usage', true, evidence);
  }
  try {
    parseUsePodUsage(parsed.data.usage);
  } catch {
    return completionFailure('UsePod review returned invalid token usage', false, evidence);
  }
  if (!choice.message.content.trim()) {
    return completionFailure('UsePod review returned empty content', true, evidence);
  }
  return {
    ok: true,
    model: parsed.data.model,
    content: choice.message.content,
    usage: parsed.data.usage,
  };
}

function parseEventStream(text: string): UsePodChatCompletion {
  const frames = eventStreamData(text);
  if (!frames.ok) return frames;

  let model: string | undefined;
  let requestId: string | undefined;
  let requestIdPresent: boolean | undefined;
  let usage: unknown;
  let usageSeen = false;
  let finished = false;
  let done = false;
  const content: string[] = [];

  for (const data of frames.data) {
    if (data === '[DONE]') {
      if (done)
        return streamFailure(
          'UsePod review stream contained duplicate terminators',
          true,
          model,
          usage,
        );
      done = true;
      continue;
    }
    if (done)
      return streamFailure(
        'UsePod review stream continued after its terminator',
        true,
        model,
        usage,
      );

    let value: unknown;
    try {
      value = JSON.parse(data) as unknown;
    } catch {
      return streamFailure('UsePod review stream contained malformed JSON', true, model, usage);
    }
    const evidence = responseEvidence(value);
    model ??= evidence.model;
    if (evidence.usage !== undefined && !usageSeen) usage = evidence.usage;

    if (!isRecord(value) || !validModelId(value.model)) {
      return streamFailure(
        'UsePod review stream contained an invalid model identity',
        false,
        model,
        usage,
      );
    }
    if (Object.hasOwn(value, 'id') && !requestIdSchema.safeParse(value.id).success) {
      return streamFailure(
        'UsePod review stream contained an invalid request identity',
        false,
        model,
        usage,
      );
    }

    const chunk = streamChunkSchema.safeParse(value);
    if (!chunk.success) {
      return streamFailure('UsePod review stream contained a malformed chunk', true, model, usage);
    }
    if (model !== chunk.data.model) {
      return streamFailure('UsePod review stream changed model identity', false, model, usage);
    }
    const hasRequestId = chunk.data.id !== undefined;
    if (chunk.data.choices.length > 0) {
      if (requestIdPresent === undefined) requestIdPresent = hasRequestId;
      if (requestIdPresent !== hasRequestId) {
        return streamFailure('UsePod review stream changed request identity', false, model, usage);
      }
    }
    if (requestId && chunk.data.id && requestId !== chunk.data.id) {
      return streamFailure('UsePod review stream changed request identity', false, model, usage);
    }
    requestId ??= chunk.data.id;

    const hasUsage = chunk.data.usage !== undefined && chunk.data.usage !== null;
    if (hasUsage) {
      if (usageSeen && !sameJsonValue(usage, chunk.data.usage)) {
        return streamFailure(
          'UsePod review stream contained conflicting usage',
          false,
          model,
          usage,
        );
      }
      if (!usageSeen) {
        usage = chunk.data.usage;
        usageSeen = true;
      }
    }

    if (chunk.data.choices.length === 0) {
      if (!hasUsage || !finished) {
        return streamFailure(
          'UsePod review stream contained an invalid usage chunk',
          false,
          model,
          usage,
        );
      }
      continue;
    }
    if (hasUsage) {
      return streamFailure(
        'UsePod review stream contained an invalid usage chunk',
        false,
        model,
        usage,
      );
    }
    if (finished) {
      return streamFailure('UsePod review stream continued after completion', true, model, usage);
    }
    const choice = chunk.data.choices[0]!;
    if (choice.delta.content) content.push(choice.delta.content);
    if (choice.finish_reason === 'stop') finished = true;
  }

  if (!done || !finished || !usageSeen || !model) {
    return streamFailure('UsePod review stream ended before completion', true, model, usage);
  }
  try {
    parseUsePodUsage(usage);
  } catch {
    return streamFailure('UsePod review stream returned invalid token usage', false, model, usage);
  }
  if (!content.join('').trim()) {
    return streamFailure('UsePod review stream returned empty content', true, model, usage);
  }
  return { ok: true, model, content: content.join(''), usage };
}

function eventStreamData(
  text: string,
): { ok: true; data: string[] } | Extract<UsePodChatCompletion, { ok: false }> {
  const frames: string[] = [];
  let dataLines: string[] = [];
  let frameHasFields = false;
  const flush = () => {
    if (dataLines.length > 0) frames.push(dataLines.join('\n'));
    dataLines = [];
    frameHasFields = false;
  };

  for (const line of text.split(/\r\n|\r|\n/)) {
    if (line === '') {
      flush();
      continue;
    }
    if (line.startsWith(':')) continue;
    frameHasFields = true;
    if (!line.startsWith('data:')) {
      return completionFailure('UsePod review stream contained an unsupported field', true);
    }
    const value = line.slice(5);
    dataLines.push(value.startsWith(' ') ? value.slice(1) : value);
    if (frames.length + 1 > MAX_STREAM_FRAMES) {
      return completionFailure('UsePod review stream contained too many frames', true);
    }
  }
  if (frameHasFields || dataLines.length > 0) flush();
  if (frames.length === 0 || frames.length > MAX_STREAM_FRAMES) {
    return completionFailure('UsePod review stream contained no completion frames', true);
  }
  return { ok: true, data: frames };
}

function responseEvidence(value: unknown): { model?: string; usage?: unknown } {
  if (!isRecord(value)) return {};
  const model = validModelId(value.model) ? value.model : undefined;
  return {
    ...(model ? { model } : {}),
    ...(value.usage === undefined || value.usage === null ? {} : { usage: value.usage }),
  };
}

function streamFailure(
  error: string,
  retryable: boolean,
  model?: string,
  usage?: unknown,
): Extract<UsePodChatCompletion, { ok: false }> {
  return completionFailure(error, retryable, {
    ...(model ? { model } : {}),
    ...(usage === undefined ? {} : { usage }),
  });
}

function completionFailure(
  error: string,
  retryable: boolean,
  evidence: { model?: string; usage?: unknown } = {},
): Extract<UsePodChatCompletion, { ok: false }> {
  return { ok: false, error, retryable, ...evidence };
}

function normalizeResponseStart(value: string): string {
  return value
    .trimStart()
    .replace(/^\uFEFF/, '')
    .trimStart();
}

function topLevelObjectKeys(value: string): string[] {
  const keys: string[] = [];
  const stack: string[] = [];
  let previous = '';
  for (let index = 0; index < value.length; index += 1) {
    const character = value[index]!;
    if (/\s/.test(character)) continue;
    if (character === '"') {
      const start = index;
      for (index += 1; index < value.length; index += 1) {
        if (value[index] === '\\') {
          index += 1;
          continue;
        }
        if (value[index] === '"') break;
      }
      if (index >= value.length) throw new Error('unterminated JSON string');
      if (stack.length === 1 && stack[0] === '{' && (previous === '{' || previous === ',')) {
        keys.push(JSON.parse(value.slice(start, index + 1)) as string);
      }
      previous = 'string';
      continue;
    }
    if (character === '{' || character === '[') stack.push(character);
    if (character === '}' || character === ']') stack.pop();
    previous = character;
  }
  return keys;
}

function sameJsonValue(left: unknown, right: unknown): boolean {
  if (Object.is(left, right)) return true;
  if (Array.isArray(left) || Array.isArray(right)) {
    return (
      Array.isArray(left) &&
      Array.isArray(right) &&
      left.length === right.length &&
      left.every((value, index) => sameJsonValue(value, right[index]))
    );
  }
  if (!isRecord(left) || !isRecord(right)) return false;
  const leftKeys = Object.keys(left).sort();
  const rightKeys = Object.keys(right).sort();
  return (
    leftKeys.length === rightKeys.length &&
    leftKeys.every((key, index) => key === rightKeys[index] && sameJsonValue(left[key], right[key]))
  );
}

function optionalHeader(response: Response, name: string): string | undefined {
  const value = response.headers.get(name)?.trim();
  return value || undefined;
}

function optionalReceiptId(response: Response, name: string): string | undefined {
  const value = optionalHeader(response, name);
  if (value && !/^[A-Za-z0-9][A-Za-z0-9._:/-]{0,127}$/.test(value)) {
    throw new UsePodReceiptError(`UsePod returned an invalid ${name} receipt`, false);
  }
  return value;
}

function positiveDecimal(value: string): boolean {
  return validDecimal(value) && /[1-9]/.test(value);
}

function decimalAtLeast(value: string, floor: string): boolean {
  if (!positiveDecimal(value) || !positiveDecimal(floor)) return false;
  const [valueWhole, valueFraction = ''] = value.split('.');
  const [floorWhole, floorFraction = ''] = floor.split('.');
  const scale = Math.max(valueFraction.length, floorFraction.length);
  const left = BigInt(`${valueWhole}${valueFraction.padEnd(scale, '0')}`);
  const right = BigInt(`${floorWhole}${floorFraction.padEnd(scale, '0')}`);
  return left >= right;
}

function validDecimal(value: string): boolean {
  return /^\d{1,48}(?:\.\d{1,18})?$/.test(value);
}

function validCostMicrounits(value: string): boolean {
  return /^\d{1,16}$/.test(value) && BigInt(value) <= MAX_RECEIPT_MICROUNITS;
}

function divideCeil(numerator: bigint, denominator: bigint): bigint {
  return (numerator + denominator - 1n) / denominator;
}

function safeTokenCount(value: unknown): value is number {
  return typeof value === 'number' && Number.isSafeInteger(value) && value >= 0;
}

function validModelId(value: unknown): value is string {
  return modelIdSchema.safeParse(value).success;
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null && !Array.isArray(value);
}

class ResponseLimitError extends Error {}
