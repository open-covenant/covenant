/**
 * Error types shared by every module.
 *
 * Every error carries a stable `code` for surfaces to switch on and a `reason`
 * written for the person reading it. HTTP and MCP return `{ error, reason }`.
 */

export type DeskErrorCode =
  | 'not_implemented'
  | 'config_invalid'
  | 'invalid_request'
  | 'invalid_state'
  | 'keystore_missing'
  | 'keystore_permissions'
  | 'not_found'
  | 'bounds_exceeded'
  | 'trap_pool'
  | 'no_reference'
  | 'upstream_failed'
  | 'live_disabled'
  | 'store_failed'
  | 'unauthorized';

/** Base class. `error` in a surface response is the code, `reason` is the text. */
export class DeskError extends Error {
  readonly code: DeskErrorCode;
  readonly reason: string;
  readonly detail: Record<string, unknown>;

  constructor(code: DeskErrorCode, reason: string, detail: Record<string, unknown> = {}) {
    super(`${code}: ${reason}`);
    this.name = new.target.name;
    this.code = code;
    this.reason = reason;
    this.detail = detail;
  }

  /** Shape returned by the HTTP and MCP surfaces. */
  toJSON(): { error: DeskErrorCode; reason: string; detail?: Record<string, unknown> } {
    return Object.keys(this.detail).length > 0
      ? { error: this.code, reason: this.reason, detail: this.detail }
      : { error: this.code, reason: this.reason };
  }
}

/** Thrown by an interface that has been declared but not built yet. */
export class NotImplementedError extends DeskError {
  constructor(what: string) {
    super('not_implemented', `${what} is not implemented yet.`, { what });
  }
}

/** Configuration file missing a field, or holding a value outside its range. */
export class ConfigError extends DeskError {
  constructor(reason: string, detail: Record<string, unknown> = {}) {
    super('config_invalid', reason, detail);
  }
}

/** A well-formed request the desk will not accept: a field is missing or out of range. */
export class InvalidRequestError extends DeskError {
  constructor(reason: string, detail: Record<string, unknown> = {}) {
    super('invalid_request', reason, detail);
  }
}

/** The request is valid but the thing it names is in a state that cannot answer it. */
export class InvalidStateError extends DeskError {
  constructor(reason: string, detail: Record<string, unknown> = {}) {
    super('invalid_state', reason, detail);
  }
}

/** A key was requested and the keystore could not supply it. */
export class KeystoreError extends DeskError {
  constructor(reason: string, detail: Record<string, unknown> = {}) {
    super('keystore_missing', reason, detail);
  }
}

/** A named token, pool, order, or market does not exist. */
export class NotFoundError extends DeskError {
  constructor(what: string, detail: Record<string, unknown> = {}) {
    super('not_found', `${what} was not found.`, detail);
  }
}

/**
 * A safety bound refused the action. The message names the limit and the value
 * that tripped it, because a refusal without a number is not actionable.
 */
export class BoundsExceededError extends DeskError {
  constructor(bound: string, limit: number, actual: number, unit: string) {
    super('bounds_exceeded', `${bound} is ${limit} ${unit} and this order needs ${actual} ${unit}.`, {
      bound,
      limit,
      actual,
      unit,
    });
  }
}

/** Routing refused a pool charging more than 300 bps. */
export class TrapPoolError extends DeskError {
  constructor(poolId: string, lpFeeBps: number) {
    super('trap_pool', `Pool ${poolId} charges ${lpFeeBps} bps and is above the 300 bps routing limit.`, {
      poolId,
      lpFeeBps,
    });
  }
}

/** No usable price for this symbol from any source. */
export class NoReferenceError extends DeskError {
  constructor(symbol: string, tried: readonly string[]) {
    super('no_reference', `No reference price for ${symbol}. Tried: ${tried.join(', ')}.`, { symbol, tried });
  }
}

/** An RPC or HTTP dependency failed. */
export class UpstreamError extends DeskError {
  constructor(service: string, reason: string, detail: Record<string, unknown> = {}) {
    super('upstream_failed', `${service}: ${reason}`, { service, ...detail });
  }
}

/** Signed execution was requested while the desk is in dry run. */
export class LiveDisabledError extends DeskError {
  constructor(reason: string) {
    super('live_disabled', reason);
  }
}

/** True when the value is a DeskError. */
export function isDeskError(value: unknown): value is DeskError {
  return value instanceof DeskError;
}

/** Turn anything thrown into the `{ error, reason }` shape a surface returns. */
export function toErrorResponse(value: unknown): { error: string; reason: string; detail?: Record<string, unknown> } {
  if (isDeskError(value)) return value.toJSON();
  if (value instanceof Error) return { error: 'upstream_failed', reason: value.message };
  return { error: 'upstream_failed', reason: String(value) };
}
