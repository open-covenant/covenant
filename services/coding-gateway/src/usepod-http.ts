import type { AccountedProviderReceipt, ProviderReceipt } from './types.js';

export interface UsePodRequestConfig {
  baseUrl: string;
  token: string;
  model: string;
  maxInputPriceMicrounits: number;
  maxOutputPriceMicrounits: number;
  minimumBalance?: string;
}

export interface UsePodUsage {
  promptTokens: number;
  completionTokens: number;
}

const MICROUNITS_PER_USD = 1_000_000n;
const MAX_RECEIPT_MICROUNITS = BigInt(Math.floor(Number.MAX_SAFE_INTEGER / 30));
const MAX_CATALOG_BYTES = 1_048_576;
const MAX_CATALOG_MODELS = 10_000;
const MAX_BALANCE_BYTES = 16_384;

export function usePodUrl(config: UsePodRequestConfig, path: string): string {
  return usePodProxyUrl(config, `v1/${path.replace(/^\/+/, '')}`);
}

export function usePodBalanceUrl(config: UsePodRequestConfig): string {
  return usePodProxyUrl(config, 'balance');
}

function usePodProxyUrl(config: UsePodRequestConfig, path: string): string {
  const base = new URL(config.baseUrl);
  if (base.protocol !== 'https:' || base.username || base.password || base.search || base.hash) {
    throw new Error('USEPOD_BASE_URL must be HTTPS and contain no credentials, query, or fragment');
  }
  const prefix = base.pathname.replace(/\/+$/, '').replace(/\/v1$/, '');
  if (prefix.includes('/proxy/')) {
    throw new Error('USEPOD_BASE_URL must not contain a tokenized proxy path');
  }
  base.pathname = `${prefix}/proxy/${encodeURIComponent(config.token)}/${path}`;
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

export function providerReceipt(
  response: Response,
  model: string,
  minimumBalance = '1',
): ProviderReceipt {
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
  const providerReportedCostMicrounits = optionalHeader(response, 'x-balance-cost-microunits');
  if (providerReportedCostMicrounits && !validCostMicrounits(providerReportedCostMicrounits)) {
    throw new Error('UsePod returned an invalid cost receipt');
  }

  return {
    model,
    route,
    balanceRemaining,
    ...(providerId ? { providerId } : {}),
    ...(requestId ? { requestId } : {}),
    ...(providerReportedCostMicrounits ? { providerReportedCostMicrounits } : {}),
  };
}

export function accountUsePodTurn(
  receipt: ProviderReceipt,
  usage: UsePodUsage,
  inputPriceMicrounitsPerMillion: number,
  outputPriceMicrounitsPerMillion: number,
): AccountedProviderReceipt {
  const ceilingCost = usePodCeilingCostMicrounits(
    usage,
    inputPriceMicrounitsPerMillion,
    outputPriceMicrounitsPerMillion,
  );
  const reported = receipt.providerReportedCostMicrounits;
  const accountedCost = reported ? maxBigInt(ceilingCost, BigInt(reported)) : ceilingCost;

  return {
    ...receipt,
    accounting: {
      accountedCostMicrounits: accountedCost.toString(),
      basis: reported
        ? 'max-of-configured-price-ceilings-and-provider-report'
        : 'configured-price-ceilings',
      inputTokens: usage.promptTokens,
      outputTokens: usage.completionTokens,
      inputPriceMicrounitsPerMillion,
      outputPriceMicrounitsPerMillion,
    },
  };
}

export function usePodCeilingCostMicrounits(
  usage: UsePodUsage,
  inputPriceMicrounitsPerMillion: number,
  outputPriceMicrounitsPerMillion: number,
): bigint {
  if (!safeTokenCount(usage.promptTokens) || !safeTokenCount(usage.completionTokens)) {
    throw new Error('UsePod returned invalid token usage');
  }
  for (const [name, value] of [
    ['input price', inputPriceMicrounitsPerMillion],
    ['output price', outputPriceMicrounitsPerMillion],
  ] as const) {
    if (!Number.isSafeInteger(value) || value <= 0) {
      throw new Error(`UsePod ${name} must be a positive safe integer`);
    }
  }
  const input = divideCeil(
    BigInt(usage.promptTokens) * BigInt(inputPriceMicrounitsPerMillion),
    MICROUNITS_PER_USD,
  );
  const output = divideCeil(
    BigInt(usage.completionTokens) * BigInt(outputPriceMicrounitsPerMillion),
    MICROUNITS_PER_USD,
  );
  return input + output;
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

export async function probeUsePodCatalog(
  config: UsePodRequestConfig,
  request: typeof fetch = fetch,
): Promise<void> {
  const response = await request(usePodUrl(config, 'models'), {
    method: 'GET',
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

  const catalog = await boundedJson(response);
  if (
    !isRecord(catalog) ||
    catalog.object !== 'list' ||
    !Array.isArray(catalog.data) ||
    catalog.data.length < 1 ||
    catalog.data.length > MAX_CATALOG_MODELS ||
    !catalog.data.every(
      (entry) =>
        isRecord(entry) &&
        typeof entry.id === 'string' &&
        entry.id.length > 0 &&
        entry.id.length <= 256,
    )
  ) {
    throw new Error('UsePod model catalog returned malformed JSON');
  }
  if (!catalog.data.some((entry) => entry.id === config.model)) {
    throw new Error('UsePod model catalog does not include the configured model');
  }
}

export async function probeUsePodBalance(
  config: UsePodRequestConfig,
  request: typeof fetch = fetch,
): Promise<void> {
  const response = await request(usePodBalanceUrl(config), {
    method: 'GET',
    headers: { accept: 'application/json', 'cache-control': 'no-cache' },
    cache: 'no-store',
    redirect: 'error',
    signal: AbortSignal.timeout(15_000),
  });
  if (!response.ok) {
    throw new Error(`UsePod balance check failed with HTTP ${response.status}`);
  }
  const contentType = response.headers.get('content-type')?.split(';', 1)[0]?.trim().toLowerCase();
  if (contentType !== 'application/json') {
    throw new Error('UsePod balance check returned a non-JSON response');
  }

  const raw = await boundedBody(response, MAX_BALANCE_BYTES, 'balance check');
  const document = uniqueTopLevelObject(raw, 'UsePod balance check');
  const bodyBalance = document.usdc_balance;
  if (typeof bodyBalance !== 'number' || !Number.isSafeInteger(bodyBalance) || bodyBalance < 0) {
    throw new Error('UsePod balance check returned invalid USDC microunits');
  }

  const headerBalance = response.headers.has('x-balance-remaining')
    ? requiredAtomicHeader(response, 'x-balance-remaining')
    : undefined;
  const bodyMicrounits = BigInt(bodyBalance);
  if (headerBalance && bodyMicrounits !== BigInt(headerBalance)) {
    throw new Error('UsePod balance evidence conflicts between body and header');
  }
  const minimum = config.minimumBalance ?? '1';
  if (!validBalanceFloor(minimum)) {
    throw new Error('USEPOD_MIN_BALANCE must be a positive whole number of USDC microunits');
  }
  if (bodyMicrounits < BigInt(minimum)) {
    throw new Error('UsePod balance is below the configured funded-balance floor');
  }
}

async function boundedJson(response: Response): Promise<unknown> {
  const raw = await boundedBody(response, MAX_CATALOG_BYTES, 'model catalog');
  try {
    return JSON.parse(raw) as unknown;
  } catch {
    throw new Error('UsePod model catalog returned malformed JSON');
  }
}

async function boundedBody(response: Response, maxBytes: number, label: string): Promise<string> {
  const contentLength = response.headers.get('content-length')?.trim();
  if (contentLength) {
    if (!/^\d+$/.test(contentLength) || BigInt(contentLength) > BigInt(maxBytes)) {
      throw new Error(`UsePod ${label} exceeded the response size limit`);
    }
  }
  if (!response.body) throw new Error(`UsePod ${label} returned malformed JSON`);

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
        throw new Error(`UsePod ${label} exceeded the response size limit`);
      }
      chunks.push(decoder.decode(value, { stream: true }));
    }
    chunks.push(decoder.decode());
  } finally {
    reader.releaseLock();
  }
  return chunks.join('');
}

function uniqueTopLevelObject(raw: string, label: string): Record<string, unknown> {
  let document: unknown;
  try {
    document = JSON.parse(raw) as unknown;
  } catch {
    throw new Error(`${label} returned malformed JSON`);
  }
  if (!isRecord(document)) throw new Error(`${label} returned malformed JSON`);

  const keys = topLevelKeys(raw);
  if (keys.length !== new Set(keys).size) {
    throw new Error(`${label} returned duplicate JSON fields`);
  }
  return document;
}

function topLevelKeys(raw: string): string[] {
  let cursor = skipWhitespace(raw, 0);
  if (raw[cursor] !== '{') return [];
  cursor = skipWhitespace(raw, cursor + 1);
  const keys: string[] = [];
  while (raw[cursor] !== '}') {
    const key = jsonString(raw, cursor);
    keys.push(key.value);
    cursor = skipWhitespace(raw, key.end);
    if (raw[cursor] !== ':') return [];
    cursor = skipJsonValue(raw, skipWhitespace(raw, cursor + 1));
    cursor = skipWhitespace(raw, cursor);
    if (raw[cursor] === '}') break;
    if (raw[cursor] !== ',') return [];
    cursor = skipWhitespace(raw, cursor + 1);
  }
  return keys;
}

function skipJsonValue(raw: string, start: number): number {
  if (raw[start] === '"') return jsonString(raw, start).end;
  if (raw[start] !== '{' && raw[start] !== '[') {
    let cursor = start;
    while (cursor < raw.length && raw[cursor] !== ',' && raw[cursor] !== '}') cursor += 1;
    return cursor;
  }

  let depth = 0;
  for (let cursor = start; cursor < raw.length; cursor += 1) {
    if (raw[cursor] === '"') {
      cursor = jsonString(raw, cursor).end - 1;
      continue;
    }
    if (raw[cursor] === '{' || raw[cursor] === '[') depth += 1;
    if (raw[cursor] === '}' || raw[cursor] === ']') depth -= 1;
    if (depth === 0) return cursor + 1;
  }
  return raw.length;
}

function jsonString(raw: string, start: number): { value: string; end: number } {
  let escaped = false;
  for (let cursor = start + 1; cursor < raw.length; cursor += 1) {
    if (!escaped && raw[cursor] === '"') {
      return {
        value: JSON.parse(raw.slice(start, cursor + 1)) as string,
        end: cursor + 1,
      };
    }
    if (!escaped && raw[cursor] === '\\') {
      escaped = true;
      continue;
    }
    escaped = false;
  }
  throw new Error('invalid JSON string');
}

function skipWhitespace(raw: string, start: number): number {
  let cursor = start;
  while (/\s/.test(raw[cursor] ?? '')) cursor += 1;
  return cursor;
}

function requiredAtomicHeader(response: Response, name: string): string {
  const value = response.headers.get(name)?.trim();
  if (!value || !/^(?:0|[1-9]\d{0,47})$/.test(value)) {
    throw new Error(`UsePod balance check returned invalid or duplicate ${name} evidence`);
  }
  return value;
}

function optionalHeader(response: Response, name: string): string | undefined {
  const value = response.headers.get(name)?.trim();
  return value || undefined;
}

function positiveDecimal(value: string): boolean {
  if (!validDecimal(value)) return false;
  return /[1-9]/.test(value);
}

export function validBalanceFloor(value: string): boolean {
  return /^[1-9]\d{0,47}$/.test(value);
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

export function validCostMicrounits(value: string): boolean {
  return /^\d{1,16}$/.test(value) && BigInt(value) <= MAX_RECEIPT_MICROUNITS;
}

function divideCeil(numerator: bigint, denominator: bigint): bigint {
  return (numerator + denominator - 1n) / denominator;
}

function maxBigInt(left: bigint, right: bigint): bigint {
  return left > right ? left : right;
}

function safeTokenCount(value: unknown): value is number {
  return typeof value === 'number' && Number.isSafeInteger(value) && value >= 0;
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null && !Array.isArray(value);
}
