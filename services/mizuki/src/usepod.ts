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

const MICROUNITS_PER_USD = 1_000_000n;
const MAX_RECEIPT_MICROUNITS = BigInt(Number.MAX_SAFE_INTEGER);
const MAX_CATALOG_BYTES = 1_048_576;
const MAX_CATALOG_MODELS = 10_000;
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
): UsePodRouteReceipt {
  const route = response.headers.get('x-pod-route')?.trim().toLowerCase();
  if (route !== 'marketplace') {
    throw new Error(`UsePod returned an unacceptable route: ${route || 'missing'}`);
  }
  const balanceRemaining = response.headers.get('x-balance-remaining')?.trim();
  if (!balanceRemaining || !decimalAtLeast(balanceRemaining, minimumBalance)) {
    if (balanceRemaining && positiveDecimal(balanceRemaining)) {
      throw new Error('UsePod balance is below the configured funded-balance floor');
    }
    throw new Error('UsePod did not prove a funded balance after the request');
  }

  const providerId = optionalHeader(response, 'x-pod-provider-id');
  const requestId = optionalHeader(response, 'x-request-id');
  const costMicrounits = optionalHeader(response, 'x-balance-cost-microunits');
  if (costMicrounits && !validCostMicrounits(costMicrounits)) {
    throw new Error('UsePod returned an invalid cost receipt');
  }
  return {
    model,
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

export function publicUsePodReceipt(receipt: UsePodRouteReceipt): ProviderRouteReceipt {
  return {
    model: receipt.model,
    route: receipt.route,
    ...(receipt.providerId ? { providerId: receipt.providerId } : {}),
    ...(receipt.requestId ? { requestId: receipt.requestId } : {}),
    ...(receipt.costMicrounits ? { costMicrounits: receipt.costMicrounits } : {}),
  };
}

export async function probeUsePodCatalog(
  config: UsePodRequestConfig,
  request: typeof fetch = fetch,
): Promise<void> {
  const response = await request(usePodUrl(config, 'models'), {
    method: 'GET',
    redirect: 'error',
    headers: { accept: 'application/json' },
    signal: AbortSignal.timeout(15_000),
  });
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

function optionalHeader(response: Response, name: string): string | undefined {
  const value = response.headers.get(name)?.trim();
  return value || undefined;
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

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null && !Array.isArray(value);
}
