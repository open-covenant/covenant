import type { Job, Quote } from './types';

const recoveryStorageKey = 'mizuki:workbench:payment-recovery';
const volatilePaymentKeys = new Map<string, string>();

type PaymentStorage = Pick<Storage, 'getItem' | 'removeItem' | 'setItem'>;

export type QuotePaymentStatus =
  | { status: 'job_reserved'; job: Job }
  | { status: 'unpaid'; expiresAt: string };

export type WorkbenchPaymentRecovery = {
  phase: 'uncertain' | 'unpaid';
  repository: string;
  issueUrl: string;
  quote: Quote;
};

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

export function paymentKey(quoteId: string): string {
  const storageKey = `mizuki:payment:${quoteId}`;
  const storage = sessionStorage();
  const existing = storage?.getItem(storageKey) ?? volatilePaymentKeys.get(quoteId);
  if (existing) return existing;
  const key = crypto.randomUUID();
  volatilePaymentKeys.set(quoteId, key);
  storage?.setItem(storageKey, key);
  return key;
}

export async function checkQuotePaymentStatus(
  quoteId: string,
  idempotencyKey: string,
  request: typeof fetch = fetch,
): Promise<QuotePaymentStatus> {
  const response = await request(
    `/api/mizuki/v1/account/quotes/${encodeURIComponent(quoteId)}/payment-status`,
    {
      method: 'GET',
      cache: 'no-store',
      credentials: 'include',
      headers: {
        accept: 'application/json',
        'idempotency-key': idempotencyKey,
      },
    },
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

export function saveWorkbenchPaymentRecovery(
  recovery: WorkbenchPaymentRecovery,
  storage: PaymentStorage | undefined = sessionStorage(),
): void {
  try {
    storage?.setItem(recoveryStorageKey, JSON.stringify(recovery));
  } catch {
    // Recovery remains available in the current view when browser storage is unavailable.
  }
}

export function loadWorkbenchPaymentRecovery(
  storage: PaymentStorage | undefined = sessionStorage(),
): WorkbenchPaymentRecovery | null {
  let raw: string | null = null;
  try {
    raw = storage?.getItem(recoveryStorageKey) ?? null;
  } catch {
    return null;
  }
  if (!raw) return null;
  try {
    const value = JSON.parse(raw) as unknown;
    if (isWorkbenchPaymentRecovery(value)) return value;
  } catch {
    // Invalid browser state is discarded below.
  }
  clearWorkbenchPaymentRecovery(undefined, storage);
  return null;
}

export function clearWorkbenchPaymentRecovery(
  quoteId?: string,
  storage: PaymentStorage | undefined = sessionStorage(),
): void {
  try {
    if (quoteId) {
      const current = loadWorkbenchPaymentRecovery(storage);
      if (current && current.quote.id !== quoteId) return;
    }
    storage?.removeItem(recoveryStorageKey);
  } catch {
    // There is no durable recovery record to clear when storage is unavailable.
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
  if (!isRecord(value) || (value.phase !== 'uncertain' && value.phase !== 'unpaid')) return false;
  if (typeof value.repository !== 'string' || typeof value.issueUrl !== 'string') return false;
  if (!isQuote(value.quote) || !quoteMatchesIssue(value.quote, value.issueUrl)) return false;
  return (
    value.repository.toLowerCase() === `${value.quote.owner}/${value.quote.repo}`.toLowerCase()
  );
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

function sessionStorage(): PaymentStorage | undefined {
  if (typeof window === 'undefined') return undefined;
  try {
    return window.sessionStorage;
  } catch {
    return undefined;
  }
}
