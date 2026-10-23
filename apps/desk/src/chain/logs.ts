/**
 * Block range planning for `eth_getLogs` on Robinhood Chain.
 *
 * The node refuses a query three ways: more than 10,000 matching logs, a query
 * that takes too long to run, and a block span wider than the filter allows
 * (30,000 blocks with no address, 100,000 with several values in one position,
 * named in the refusal itself). The first two are answered by halving the range
 * and asking again, so a scan converges on ranges the node will serve instead
 * of guessing a span up front. The third is answered by cutting the range into
 * the span the node said it allows.
 *
 * A third refusal, too many requests in a short window, is not a range problem.
 * It is answered by waiting.
 */

/** An inclusive block range. */
export interface BlockRange {
  readonly from: bigint;
  readonly to: bigint;
}

/** Blocks per query before splitting. Chosen from live behaviour on 4663. */
export const DEFAULT_SPAN = 2_000_000n;

/** Cut a range into consecutive spans, the last one short. */
export function chunkRange(from: bigint, to: bigint, span: bigint = DEFAULT_SPAN): BlockRange[] {
  if (span <= 0n) throw new RangeError(`span must be positive, got ${span}`);
  if (to < from) return [];
  const ranges: BlockRange[] = [];
  let cursor = from;
  while (cursor <= to) {
    const end = cursor + span - 1n;
    ranges.push({ from: cursor, to: end > to ? to : end });
    cursor = end + 1n;
  }
  return ranges;
}

/**
 * Halve a range. Returns null for a single block, which cannot be split and so
 * has to be reported as a failure rather than retried forever.
 */
export function splitRange(range: BlockRange): [BlockRange, BlockRange] | null {
  if (range.to <= range.from) return null;
  const mid = range.from + (range.to - range.from) / 2n;
  return [
    { from: range.from, to: mid },
    { from: mid + 1n, to: range.to },
  ];
}

/** Number of blocks in a range, inclusive. */
export function rangeSize(range: BlockRange): bigint {
  return range.to - range.from + 1n;
}

/** Every message an error carries, lowercased, so patterns match once. */
function errorText(error: unknown): string {
  const parts: string[] = [];
  let cursor: unknown = error;
  for (let depth = 0; cursor && depth < 5; depth += 1) {
    const record = cursor as {
      message?: unknown;
      details?: unknown;
      shortMessage?: unknown;
      cause?: unknown;
    };
    for (const field of [record.message, record.details, record.shortMessage]) {
      if (typeof field === 'string') parts.push(field);
    }
    cursor = record.cause;
  }
  if (parts.length === 0) parts.push(String(error));
  return parts.join(' | ').toLowerCase();
}

/** The node refused because the query matched more than 10,000 logs. */
export function isLogLimitError(error: unknown): boolean {
  return /exceeds limit of \d+|more than \d+ results|query returned more than/.test(
    errorText(error),
  );
}

/** The node gave up on the query before it finished. */
export function isQueryTimeoutError(error: unknown): boolean {
  return /log query timed out|query timeout|timeout exceeded|timed out/.test(errorText(error));
}

/**
 * The block span the node allows for this filter, when it refused a query for
 * spanning more: "query spans 2000000 blocks (...), but only 30000 are allowed
 * for this request".
 */
export function spanLimitOf(error: unknown): bigint | undefined {
  const match = /query spans \d+ blocks.*only (\d+) are allowed/.exec(errorText(error));
  if (!match?.[1]) return undefined;
  const allowed = BigInt(match[1]);
  return allowed > 0n ? allowed : undefined;
}

/** The node refused because the query spans more blocks than it allows. */
export function isSpanLimitError(error: unknown): boolean {
  return spanLimitOf(error) !== undefined;
}

/** The refusal is answered by narrowing the range. */
export function isRangeTooWideError(error: unknown): boolean {
  return isLogLimitError(error) || isQueryTimeoutError(error) || isSpanLimitError(error);
}

/** The refusal is answered by waiting, not by narrowing. */
export function isRateLimitError(error: unknown): boolean {
  const text = errorText(error);
  if (/too many requests|rate limit|429/.test(text)) return true;
  if (isChallengeError(error)) return true;
  const code = (error as { code?: unknown } | null)?.code;
  return code === 429 || code === -32005;
}

/**
 * The endpoint answered with a browser challenge instead of a result.
 *
 * The Robinhood Chain RPC sits behind Cloudflare, and a client asking faster
 * than it likes gets a 403 challenge page rather than a 429. Reading that as a
 * hard failure would end a pool scan halfway through; reading it as a signal to
 * slow down lets the scan finish.
 */
export function isChallengeError(error: unknown): boolean {
  const text = errorText(error);
  const forbidden = /status: 403|http 403/.test(text) || statusOf(error) === 403;
  if (!forbidden) return false;
  return /just a moment|cf_chl|cdn-cgi\/challenge|cf-mitigated/.test(text);
}

/** HTTP status an error carries, at any depth of its causes. */
function statusOf(error: unknown): number | undefined {
  let cursor: unknown = error;
  for (let depth = 0; cursor && depth < 5; depth += 1) {
    const record = cursor as { status?: unknown; cause?: unknown };
    if (typeof record.status === 'number') return record.status;
    cursor = record.cause;
  }
  return undefined;
}
