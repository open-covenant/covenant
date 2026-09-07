/**
 * Shared plumbing for the order book and the engine: identifiers, the audit
 * trail, and the small conversions both sides need.
 */

import { randomBytes } from 'node:crypto';
import type { Address, Order, OrderStatus } from '../core/types.js';
import type { Config } from '../core/config.js';
import type { Logger } from '../core/logger.js';
import type { Store } from '../core/store.js';
import type { ChainModule } from '../chain/index.js';
import type { FairValueModule } from '../fairvalue/index.js';
import type { SnapshotProvider } from './types.js';

/** Everything the book and the engine share, with defaults already applied. */
export interface OrdersContext {
  readonly config: Config;
  readonly logger: Logger;
  readonly store: Store;
  readonly chain: ChainModule;
  readonly fairvalue: FairValueModule;
  readonly snapshots: SnapshotProvider;
  readonly now: () => number;
}


/** Rolling window the daily notional cap is measured over. */
export const DAY_MS = 24 * 60 * 60 * 1000;

/** Every audit row the module writes. */
export type OrderEventKind =
  | 'order_created'
  | 'order_triggered'
  | 'order_filled'
  | 'order_failed'
  | 'order_cancelled'
  | 'order_expired'
  | 'order_refused';

const STATUS_EVENT: Record<OrderStatus, OrderEventKind> = {
  open: 'order_created',
  triggered: 'order_triggered',
  filled: 'order_filled',
  failed: 'order_failed',
  cancelled: 'order_cancelled',
  expired: 'order_expired',
};

/** Short, sortable-enough identifier. Twelve random bytes behind a prefix. */
export function newId(prefix: string): string {
  return `${prefix}_${randomBytes(9).toString('hex')}`;
}

/** The token whose price the trader is waiting on: what a buy receives, what a sell gives up. */
export function subjectToken(order: Pick<Order, 'side' | 'tokenIn' | 'tokenOut'>): Address {
  return order.side === 'buy' ? order.tokenOut : order.tokenIn;
}

/**
 * Decimals for a token, or undefined when nothing authoritative says.
 *
 * The size of a fill in USD is what every notional cap is measured against, so
 * a guess here is a cap that does not hold. The chain reader answers from the
 * token itself and caches only what it read, so a token that has never
 * answered comes back undefined and the order is refused instead of sized
 * wrong.
 */
export async function decimalsFor(ctx: OrdersContext, address: Address): Promise<number | undefined> {
  const stored = ctx.store.getToken(address)?.decimals;
  if (stored !== undefined) return stored;
  try {
    const read = await ctx.chain.tokens.decimalsOf([address]);
    return read.get(address.toLowerCase());
  } catch (error) {
    ctx.logger.debug('token decimals could not be read', {
      token: address,
      reason: error instanceof Error ? error.message : String(error),
    });
    return undefined;
  }
}

/** Move an order to a new status, write the audit row, and return the stored order. */
export function transition(
  ctx: OrdersContext,
  order: Order,
  status: OrderStatus,
  reason?: string,
  detail: Record<string, unknown> = {},
): Order {
  ctx.store.updateOrderStatus(order.id, status, reason);
  ctx.store.appendEvent({
    ts: ctx.now(),
    kind: STATUS_EVENT[status],
    subject: order.id,
    detail: { from: order.status, to: status, ...(reason ? { reason } : {}), ...detail },
  });
  ctx.logger.info('order status changed', { order: order.id, from: order.status, to: status, reason });
  return ctx.store.getOrder(order.id) ?? { ...order, status, reason };
}

/** Write an audit row without changing the status. */
export function note(
  ctx: OrdersContext,
  kind: OrderEventKind,
  subject: string,
  detail: Record<string, unknown>,
): void {
  ctx.store.appendEvent({ ts: ctx.now(), kind, subject, detail });
}

/**
 * USD for a refusal or a trigger message, no currency symbol.
 *
 * Two decimals for anything a person would read as money, more for the tokens
 * that trade below a cent: a message reading "the price is 0.00 USD, at or
 * below 0.00 USD" tells the trader nothing.
 */
export function usd(value: number): string {
  if (!Number.isFinite(value)) return String(value);
  const size = Math.abs(value);
  if (size >= 0.01 || size === 0) return value.toFixed(2);
  return value.toFixed(Math.min(18, Math.ceil(-Math.log10(size)) + 3));
}

/** Basis points for a refusal message: whole bps unless the value is finer. */
export function bps(value: number): string {
  return Number.isInteger(value) ? String(value) : value.toFixed(1);
}

/** True while the order can still change. */
export function isActive(status: OrderStatus): boolean {
  return status === 'open' || status === 'triggered';
}
