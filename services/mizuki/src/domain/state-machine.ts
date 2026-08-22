export type TransitionTable<State extends string> = Readonly<Record<State, readonly State[]>>;

export class DomainRuleError extends Error {
  readonly code: string;

  constructor(code: string, message: string) {
    super(message);
    this.name = 'DomainRuleError';
    this.code = code;
  }
}

export function canTransition<State extends string>(
  table: TransitionTable<State>,
  from: State,
  to: State,
): boolean {
  return table[from].includes(to);
}

export function assertTransition<State extends string>(
  table: TransitionTable<State>,
  from: State,
  to: State,
  aggregate: string,
): void {
  if (!canTransition(table, from, to)) {
    throw new DomainRuleError(
      'INVALID_TRANSITION',
      `${aggregate} cannot transition from ${from} to ${to}`,
    );
  }
}

export function assertExpectedRevision(actual: number, expected: number): void {
  if (!Number.isSafeInteger(expected) || expected < 0) {
    throw new DomainRuleError(
      'INVALID_REVISION',
      'Expected revision must be a non-negative integer',
    );
  }
  if (actual !== expected) {
    throw new DomainRuleError('STALE_REVISION', `Expected revision ${expected}, found ${actual}`);
  }
}

export function timestampMs(value: string, field = 'timestamp'): number {
  const timestamp = Date.parse(value);
  if (!Number.isFinite(timestamp)) {
    throw new DomainRuleError('INVALID_TIMESTAMP', `${field} must be a valid ISO timestamp`);
  }
  return timestamp;
}

export function assertNotBefore(value: string, minimum: string, field = 'timestamp'): void {
  if (timestampMs(value, field) < timestampMs(minimum, 'minimum timestamp')) {
    throw new DomainRuleError(
      'TIME_MOVED_BACKWARD',
      `${field} cannot be earlier than the aggregate's last update`,
    );
  }
}

export function addHours(value: string, hours: number): string {
  if (!Number.isFinite(hours) || hours <= 0) {
    throw new DomainRuleError('INVALID_DURATION', 'Hours must be positive');
  }
  return new Date(timestampMs(value) + hours * 60 * 60 * 1_000).toISOString();
}

export function assertNonEmpty(value: string, field: string): string {
  const normalized = value.trim();
  if (!normalized) {
    throw new DomainRuleError('REQUIRED_FIELD', `${field} is required`);
  }
  return normalized;
}

export function assertUsdCents(value: number, field: string): number {
  if (!Number.isSafeInteger(value) || value < 0) {
    throw new DomainRuleError('INVALID_MONEY', `${field} must be non-negative integer cents`);
  }
  return value;
}
