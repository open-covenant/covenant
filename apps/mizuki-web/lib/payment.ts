import type { Job, Quote } from './types';
import {
  fetchWithDeadline,
  workbenchMutation,
  workbenchRequest,
  WorkbenchRequestError,
} from './workbench-client';

const recoveryStorageKey = 'mizuki:workbench:payment-recovery';
const paymentStatusTimeoutMs = 12_000;

type PaymentStorage = Pick<Storage, 'getItem' | 'removeItem' | 'setItem'>;

export const paymentAttemptStages = [
  'created',
  'wallet_opened',
  'wallet_signed',
  'submitting',
] as const;

export type PaymentAttemptStage = (typeof paymentAttemptStages)[number];

export type PaymentAttemptStatus =
  | PaymentAttemptStage
  | 'job_reserved'
  | 'expired_unpaid'
  | 'indeterminate';

export type PaymentAttempt = {
  id: string;
  quoteId: string;
  idempotencyKey: string;
  stage: PaymentAttemptStatus;
  paymentStatus: PaymentAttemptStatus;
  retrySafe: boolean;
  expiresAt?: string;
  job?: Job;
  requestId?: string;
  buildId?: string;
};

export type ActivePaymentAttempt = {
  attempt: PaymentAttempt | null;
  quote?: Quote;
};

export type QuotePaymentStatus =
  | { status: 'job_reserved'; job: Job }
  | { status: 'unpaid'; expiresAt: string };

export type WorkbenchPaymentRecovery = {
  phase: 'prepared' | 'attempting' | 'uncertain' | 'unpaid';
  walletAuthorized?: boolean;
  accountId: string;
  attemptId: string;
  idempotencyKey: string;
  repository: string;
  issueUrl: string;
  quote: Quote;
};

export class PaymentRecoveryStorageError extends Error {
  constructor(message = 'Secure payment recovery is unavailable in this browser tab.') {
    super(message);
  }
}

export class PaymentStatusError extends Error {
  constructor(
    message: string,
    readonly status: number,
  ) {
    super(message);
  }
}

export class PaymentAttemptBusyError extends Error {
  constructor() {
    super('This payment is already open in another Workbench tab.');
  }
}

export function paymentApplicationBuild(): string {
  return process.env.NEXT_PUBLIC_MIZUKI_BUILD_ID?.trim() || 'development';
}

export async function createPaymentAttempt(input: {
  quoteId: string;
  wallet: string;
  appBuild?: string;
}): Promise<PaymentAttempt> {
  const value = await workbenchMutation<unknown>('/v1/account/payment-attempts', {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify({
      quote_id: input.quoteId,
      wallet: input.wallet,
      app_build: input.appBuild ?? paymentApplicationBuild(),
    }),
  });
  return normalizePaymentAttempt(value, input.quoteId);
}

export async function reportPaymentAttemptStage(
  attemptId: string,
  stage: Exclude<PaymentAttemptStage, 'created'>,
): Promise<void> {
  publishPaymentAttempt({ attemptId, stage });
  await workbenchMutation(`/v1/account/payment-attempts/${encodeURIComponent(attemptId)}/stage`, {
    method: 'PATCH',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify({ stage }),
  });
}

export async function checkPaymentAttempt(
  attemptId: string,
  expectedQuoteId?: string,
): Promise<PaymentAttempt> {
  return normalizePaymentAttempt(
    await workbenchRequest<unknown>(
      `/v1/account/payment-attempts/${encodeURIComponent(attemptId)}`,
    ),
    expectedQuoteId,
  );
}

export async function findActivePaymentAttempt(): Promise<ActivePaymentAttempt> {
  const value = await workbenchRequest<unknown>('/v1/account/payment-attempts/active');
  if (!isRecord(value)) throw new Error('The active payment response was invalid');
  if (value.attempt === null && value.paymentStatus === 'none') return { attempt: null };
  const attempt = normalizePaymentAttempt(value);
  const quote = value.quote === undefined ? undefined : parseQuote(value.quote);
  return { attempt, ...(quote ? { quote } : {}) };
}

export async function reconcilePaymentAttempt(
  attemptId: string,
  expectedQuoteId: string,
  options: { signal?: AbortSignal; attempts?: number } = {},
): Promise<PaymentAttempt> {
  const attempts = Math.max(1, Math.min(options.attempts ?? 4, 6));
  let lastError: unknown;
  for (let index = 0; index < attempts; index += 1) {
    if (options.signal?.aborted) throw abortReason(options.signal);
    try {
      return await checkPaymentAttempt(attemptId, expectedQuoteId);
    } catch (cause) {
      lastError = cause;
      if (!retryableAttemptRead(cause) || index === attempts - 1) throw cause;
      await wait(attemptReadDelay(index), options.signal);
    }
  }
  throw lastError;
}

export function normalizePaymentAttempt(value: unknown, expectedQuoteId?: string): PaymentAttempt {
  const source =
    isRecord(value) && isRecord(value.attempt)
      ? {
          ...value.attempt,
          paymentStatus: value.paymentStatus ?? value.payment_status ?? value.attempt.paymentStatus,
          retrySafe: value.retrySafe ?? value.retry_safe ?? value.attempt.retrySafe,
          job: value.job ?? value.attempt.job,
          requestId: value.requestId ?? value.attempt.requestId,
          buildId: value.buildId ?? value.attempt.buildId,
        }
      : value;
  if (!isRecord(source)) throw new Error('The payment attempt response was invalid');
  const id = readBoundedId(source.id);
  const quoteId = readBoundedId(source.quoteId ?? source.quote_id);
  const idempotencyKey = readIdempotencyKey(source.idempotencyKey ?? source.idempotency_key);
  const rawStage = source.stage;
  if (!paymentAttemptStatus(rawStage)) {
    throw new Error('The payment attempt stage was invalid');
  }
  const rawStatus = source.paymentStatus ?? source.payment_status ?? source.status ?? rawStage;
  if (!paymentAttemptStatus(rawStatus)) {
    throw new Error('The payment attempt status was invalid');
  }
  if (expectedQuoteId && quoteId !== expectedQuoteId) {
    throw new Error('Payment status did not match the accepted quote');
  }
  const job = source.job === undefined ? undefined : parseJob(source.job);
  if (rawStatus === 'job_reserved' && !job) {
    throw new Error('The reserved payment attempt did not include its job');
  }
  const expiresAt = typeof source.expiresAt === 'string' ? source.expiresAt : undefined;
  if (expiresAt && Number.isNaN(Date.parse(expiresAt))) {
    throw new Error('The payment attempt expiry was invalid');
  }
  return {
    id,
    quoteId,
    idempotencyKey,
    stage: rawStage,
    paymentStatus: rawStatus,
    retrySafe: source.retrySafe === true || source.retry_safe === true,
    ...(expiresAt ? { expiresAt } : {}),
    ...(job ? { job } : {}),
    ...(typeof source.requestId === 'string' ? { requestId: source.requestId } : {}),
    ...(typeof source.buildId === 'string' ? { buildId: source.buildId } : {}),
  };
}

export async function withPaymentAttemptLock<T>(
  attemptId: string,
  operation: () => Promise<T>,
): Promise<T> {
  if (typeof navigator === 'undefined' || !navigator.locks) return operation();
  const result = await navigator.locks.request(
    `mizuki:payment-attempt:${attemptId}`,
    { ifAvailable: true },
    async (lock) => {
      if (!lock) throw new PaymentAttemptBusyError();
      publishPaymentAttempt({ attemptId, stage: 'created', active: true });
      try {
        return await operation();
      } finally {
        publishPaymentAttempt({ attemptId, stage: 'created', active: false });
      }
    },
  );
  return result;
}

export async function readJsonResponse<T>(response: Response): Promise<T> {
  const body = (await response.json().catch(() => ({}))) as T & {
    error?: string;
    reason?: string;
  };
  if (!response.ok) {
    throw new Error(body.error || body.reason || `Request failed (${response.status})`);
  }
  return body;
}

export async function checkQuotePaymentStatus(
  quoteId: string,
  idempotencyKey: string,
  options: {
    request?: typeof fetch;
    signal?: AbortSignal;
    timeoutMs?: number;
  } = {},
): Promise<QuotePaymentStatus> {
  const response = await fetchWithDeadline(
    `/api/mizuki/v1/account/quotes/${encodeURIComponent(quoteId)}/payment-status`,
    {
      method: 'GET',
      cache: 'no-store',
      credentials: 'include',
      signal: options.signal,
      headers: {
        accept: 'application/json',
        'idempotency-key': idempotencyKey,
      },
    },
    options.request ?? fetch,
    options.timeoutMs ?? paymentStatusTimeoutMs,
  );
  const body = (await response.json().catch(() => ({}))) as Record<string, unknown>;
  if (!response.ok) {
    throw new PaymentStatusError(
      typeof body.error === 'string'
        ? body.error
        : `Payment status request failed (${response.status})`,
      response.status,
    );
  }
  if (body.quoteId !== quoteId) throw new Error('Payment status did not match the accepted quote');
  if (body.paymentStatus === 'job_reserved' && isJob(body.job)) {
    return { status: 'job_reserved', job: body.job };
  }
  if (body.paymentStatus === 'unpaid' && typeof body.expiresAt === 'string') {
    return { status: 'unpaid', expiresAt: body.expiresAt };
  }
  throw new Error('Payment status response was invalid');
}

export function paymentAccountId(value: unknown): string {
  if (!isRecord(value)) throw new Error('The account response was invalid');
  const source = isRecord(value.account) ? value.account : value;
  const raw = source.githubId;
  const accountId =
    typeof raw === 'string'
      ? raw
      : typeof raw === 'number' && Number.isSafeInteger(raw)
        ? String(raw)
        : '';
  if (!/^[1-9]\d*$/.test(accountId)) {
    throw new Error('The account response did not include a stable GitHub identity');
  }
  return accountId;
}

export function prepareWorkbenchPaymentRecovery(
  input: Omit<WorkbenchPaymentRecovery, 'phase'>,
  storage: PaymentStorage | undefined = browserSessionStorage(),
): WorkbenchPaymentRecovery {
  const recovery: WorkbenchPaymentRecovery = {
    ...input,
    phase: 'prepared',
    walletAuthorized: input.walletAuthorized ?? false,
  };
  saveWorkbenchPaymentRecovery(recovery, storage);
  return recovery;
}

export function saveWorkbenchPaymentRecovery(
  recovery: WorkbenchPaymentRecovery,
  storage: PaymentStorage | undefined = browserSessionStorage(),
): boolean {
  if (!storage) return false;
  if (!isWorkbenchPaymentRecovery(recovery)) {
    return false;
  }
  const encoded = JSON.stringify(recovery);
  try {
    storage.setItem(recoveryStorageKey, encoded);
    return storage.getItem(recoveryStorageKey) === encoded;
  } catch {
    return false;
  }
}

export function loadWorkbenchPaymentRecovery(
  accountId: string,
  storage: PaymentStorage | undefined = browserSessionStorage(),
): WorkbenchPaymentRecovery | null {
  const recovery = readWorkbenchPaymentRecovery(storage);
  return recovery?.accountId === accountId ? recovery : null;
}

export function clearWorkbenchPaymentRecovery(
  accountId: string,
  quoteId?: string,
  storage: PaymentStorage | undefined = browserSessionStorage(),
): void {
  try {
    const current = readWorkbenchPaymentRecovery(storage);
    if (!current || current.accountId !== accountId) return;
    if (quoteId && current.quote.id !== quoteId) return;
    storage?.removeItem(recoveryStorageKey);
  } catch {
    return;
  }
}

export function quoteExpired(quote: Pick<Quote, 'expiresAt'>, now = Date.now()): boolean {
  const expiry = Date.parse(quote.expiresAt);
  return Number.isNaN(expiry) || expiry <= now;
}

export function quoteMatchesIssue(
  quote: Pick<Quote, 'owner' | 'repo' | 'issueNumber'>,
  issueUrl: string,
): boolean {
  const issue = githubIssueIdentity(issueUrl);
  return Boolean(
    issue &&
    issue.owner === quote.owner.toLowerCase() &&
    issue.repo === quote.repo.toLowerCase() &&
    issue.number === quote.issueNumber,
  );
}

export function issueMatchesRepository(issueUrl: string, repository: string): boolean {
  const issue = githubIssueIdentity(issueUrl);
  const parts = repository.split('/');
  if (!issue || parts.length !== 2 || !parts[0] || !parts[1]) return false;
  return issue.owner === parts[0].toLowerCase() && issue.repo === parts[1].toLowerCase();
}

function isWorkbenchPaymentRecovery(value: unknown): value is WorkbenchPaymentRecovery {
  if (
    !isRecord(value) ||
    !['prepared', 'attempting', 'uncertain', 'unpaid'].includes(String(value.phase))
  ) {
    return false;
  }
  if (typeof value.accountId !== 'string' || !/^[1-9]\d*$/.test(value.accountId)) return false;
  if (value.walletAuthorized !== undefined && typeof value.walletAuthorized !== 'boolean') {
    return false;
  }
  if (typeof value.attemptId !== 'string' || !validBoundedId(value.attemptId)) return false;
  if (typeof value.idempotencyKey !== 'string' || !validIdempotencyKey(value.idempotencyKey)) {
    return false;
  }
  if (typeof value.repository !== 'string' || typeof value.issueUrl !== 'string') return false;
  if (!isQuote(value.quote) || !quoteMatchesIssue(value.quote, value.issueUrl)) return false;
  return (
    value.repository.toLowerCase() === `${value.quote.owner}/${value.quote.repo}`.toLowerCase()
  );
}

export function paymentRetryAllowed(
  recovery: Pick<WorkbenchPaymentRecovery, 'walletAuthorized'>,
  attempt: Pick<PaymentAttempt, 'retrySafe'>,
): boolean {
  return attempt.retrySafe && recovery.walletAuthorized !== true;
}

function readWorkbenchPaymentRecovery(
  storage: PaymentStorage | undefined,
): WorkbenchPaymentRecovery | null {
  let raw: string | null;
  try {
    raw = storage?.getItem(recoveryStorageKey) ?? null;
  } catch {
    return null;
  }
  if (!raw) return null;
  let value: unknown;
  try {
    value = JSON.parse(raw) as unknown;
  } catch {
    value = null;
  }
  if (isWorkbenchPaymentRecovery(value)) return value;
  try {
    storage?.removeItem(recoveryStorageKey);
  } catch {
    return null;
  }
  return null;
}

function githubIssueIdentity(
  value: string,
): { owner: string; repo: string; number: number } | undefined {
  try {
    const url = new URL(value.trim());
    const parts = url.pathname.replace(/\/$/, '').split('/').filter(Boolean);
    if (
      url.protocol !== 'https:' ||
      url.hostname.toLowerCase() !== 'github.com' ||
      parts.length !== 4 ||
      parts[2] !== 'issues' ||
      !/^\d+$/.test(parts[3]!)
    ) {
      return undefined;
    }
    return {
      owner: parts[0]!.toLowerCase(),
      repo: parts[1]!.toLowerCase(),
      number: Number(parts[3]),
    };
  } catch {
    return undefined;
  }
}

function isQuote(value: unknown): value is Quote {
  if (!isRecord(value)) return false;
  return (
    typeof value.id === 'string' &&
    typeof value.issueUrl === 'string' &&
    typeof value.owner === 'string' &&
    typeof value.repo === 'string' &&
    Number.isInteger(value.issueNumber) &&
    typeof value.issueTitle === 'string' &&
    (value.class === 'micro' || value.class === 'standard') &&
    typeof value.priceAtomic === 'string' &&
    /^[1-9]\d*$/.test(value.priceAtomic) &&
    Number.isInteger(value.maxFiles) &&
    typeof value.maxCostUsd === 'number' &&
    typeof value.expiresAt === 'string'
  );
}

function isJob(value: unknown): value is Job {
  return isRecord(value) && typeof value.id === 'string' && typeof value.state === 'string';
}

function parseJob(value: unknown): Job | undefined {
  return isJob(value) ? value : undefined;
}

function parseQuote(value: unknown): Quote | undefined {
  return isQuote(value) ? value : undefined;
}

function paymentAttemptStatus(value: unknown): value is PaymentAttemptStatus {
  return (
    paymentAttemptStages.includes(value as PaymentAttemptStage) ||
    value === 'job_reserved' ||
    value === 'expired_unpaid' ||
    value === 'indeterminate'
  );
}

function readBoundedId(value: unknown): string {
  if (typeof value !== 'string' || !validBoundedId(value)) {
    throw new Error('The payment attempt identifier was invalid');
  }
  return value;
}

function validBoundedId(value: string): boolean {
  return /^[A-Za-z0-9][A-Za-z0-9:_-]{7,127}$/.test(value);
}

function readIdempotencyKey(value: unknown): string {
  if (typeof value !== 'string' || !validIdempotencyKey(value)) {
    throw new Error('The payment attempt idempotency key was invalid');
  }
  return value;
}

function validIdempotencyKey(value: string): boolean {
  return /^[A-Za-z0-9][A-Za-z0-9:_-]{15,127}$/.test(value);
}

function retryableAttemptRead(cause: unknown): boolean {
  return !(cause instanceof WorkbenchRequestError) || cause.status === 429 || cause.status >= 500;
}

function attemptReadDelay(index: number): number {
  const base = Math.min(2_000, 250 * 2 ** index);
  return base + Math.floor(Math.random() * Math.max(1, base / 4));
}

function wait(milliseconds: number, signal?: AbortSignal): Promise<void> {
  if (signal?.aborted) return Promise.reject(abortReason(signal));
  return new Promise((resolve, reject) => {
    const done = () => {
      signal?.removeEventListener('abort', abort);
      resolve();
    };
    const timer = setTimeout(done, milliseconds);
    const abort = () => {
      clearTimeout(timer);
      signal?.removeEventListener('abort', abort);
      reject(abortReason(signal!));
    };
    signal?.addEventListener('abort', abort, { once: true });
  });
}

function abortReason(signal: AbortSignal): unknown {
  return signal.reason ?? new DOMException('The operation was aborted', 'AbortError');
}

function publishPaymentAttempt(message: {
  attemptId: string;
  stage: PaymentAttemptStage;
  active?: boolean;
}): void {
  if (typeof BroadcastChannel === 'undefined') return;
  const channel = new BroadcastChannel('mizuki:payment-attempts');
  channel.postMessage({ ...message, at: Date.now() });
  channel.close();
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null;
}

function browserSessionStorage(): PaymentStorage | undefined {
  if (typeof window === 'undefined') return undefined;
  try {
    return window.sessionStorage;
  } catch {
    return undefined;
  }
}
