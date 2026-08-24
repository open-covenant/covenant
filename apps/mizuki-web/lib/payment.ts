import type { Quote } from './types';

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
  const existing = window.sessionStorage.getItem(storageKey);
  if (existing) return existing;
  const key = crypto.randomUUID();
  window.sessionStorage.setItem(storageKey, key);
  return key;
}

export function quoteExpired(quote: Pick<Quote, 'expiresAt'>, now = Date.now()): boolean {
  const expiry = Date.parse(quote.expiresAt);
  return Number.isNaN(expiry) || expiry <= now;
}

export function quoteMatchesIssue(
  quote: Pick<Quote, 'owner' | 'repo' | 'issueNumber'>,
  issueUrl: string,
): boolean {
  try {
    const url = new URL(issueUrl.trim());
    const parts = url.pathname.replace(/\/$/, '').split('/').filter(Boolean);
    if (
      url.protocol !== 'https:' ||
      url.hostname.toLowerCase() !== 'github.com' ||
      parts.length !== 4 ||
      parts[2] !== 'issues' ||
      !/^\d+$/.test(parts[3]!)
    ) {
      return false;
    }
    return (
      parts[0]!.toLowerCase() === quote.owner.toLowerCase() &&
      parts[1]!.toLowerCase() === quote.repo.toLowerCase() &&
      Number(parts[3]) === quote.issueNumber
    );
  } catch {
    return false;
  }
}
