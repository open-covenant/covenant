import type { Job, Quote } from './types';
import { fetchWithDeadline } from './workbench-client';

const recoveryStorageKey = 'mizuki:workbench:payment-recovery';
const paymentStatusTimeoutMs = 12_000;

type PaymentStorage = Pick<Storage, 'getItem' | 'removeItem' | 'setItem'>;

export type QuotePaymentStatus =
  | { status: 'job_reserved'; job: Job }
  | { status: 'unpaid'; expiresAt: string };

export type WorkbenchPaymentRecovery = {
  phase: 'prepared' | 'attempting' | 'uncertain' | 'unpaid';
  accountId: string;
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
  input: Omit<WorkbenchPaymentRecovery, 'idempotencyKey' | 'phase'>,
  storage: PaymentStorage | undefined = browserSessionStorage(),
): WorkbenchPaymentRecovery {
  const current = readWorkbenchPaymentRecovery(storage);
  if (current && current.accountId !== input.accountId) {
    throw new PaymentRecoveryStorageError(
      'This tab contains an unresolved payment from a different GitHub account. Sign back into that account to resolve it before paying again.',
    );
  }
  if (current && current.quote.id !== input.quote.id) {
    throw new PaymentRecoveryStorageError(
      'Resolve the existing payment status in this tab before starting another payment.',
    );
  }
  const recovery: WorkbenchPaymentRecovery = {
    ...input,
    phase: 'prepared',
    idempotencyKey: current?.idempotencyKey ?? crypto.randomUUID(),
  };
  saveWorkbenchPaymentRecovery(recovery, storage);
  return recovery;
}

export function saveWorkbenchPaymentRecovery(
  recovery: WorkbenchPaymentRecovery,
  storage: PaymentStorage | undefined = browserSessionStorage(),
): void {
  if (!storage) throw new PaymentRecoveryStorageError();
  if (!isWorkbenchPaymentRecovery(recovery)) {
    throw new PaymentRecoveryStorageError('The payment recovery record was invalid.');
  }
  const encoded = JSON.stringify(recovery);
  try {
    storage.setItem(recoveryStorageKey, encoded);
    if (storage.getItem(recoveryStorageKey) !== encoded) throw new PaymentRecoveryStorageError();
  } catch {
    throw new PaymentRecoveryStorageError();
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
  if (
    typeof value.idempotencyKey !== 'string' ||
    !/^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/i.test(
      value.idempotencyKey,
    )
  ) {
    return false;
  }
  if (typeof value.repository !== 'string' || typeof value.issueUrl !== 'string') return false;
  if (!isQuote(value.quote) || !quoteMatchesIssue(value.quote, value.issueUrl)) return false;
  return (
    value.repository.toLowerCase() === `${value.quote.owner}/${value.quote.repo}`.toLowerCase()
  );
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
    typeof value.expiresAt === 'string' &&
    value.payment !== undefined
  );
}

function isJob(value: unknown): value is Job {
  return isRecord(value) && typeof value.id === 'string' && typeof value.state === 'string';
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
