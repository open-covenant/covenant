/**
 * The order book: create, read, and cancel.
 *
 * Creation is where an order is refused for good. Anything that reaches the
 * store has a valid token pair, a trigger that can fire, bounds inside the
 * configured caps, and permission to be live if it asked to be.
 */

import { liveBlockedReason } from '../core/config.js';
import {
  BoundsExceededError,
  DeskError,
  InvalidStateError,
  LiveDisabledError,
  NotFoundError,
} from '../core/errors.js';
import type { Address, Order, OrderBounds, OrderStatus } from '../core/types.js';
import { CreateOrderInputSchema, resolveBounds, type ParsedCreateOrderInput, type ParsedLeg } from './schema.js';
import { isActive, newId, transition, type OrdersContext } from './support.js';
import type { CreateOrderInput, OrderBook } from './types.js';

/** An order the desk will not accept. The reason always names the field and the value. */
export class OrderInvalidError extends DeskError {
  constructor(reason: string, detail: Record<string, unknown> = {}) {
    super('invalid_request', reason, detail);
  }
}

export function createOrderBook(ctx: OrdersContext): OrderBook {
  return {
    async create(input: CreateOrderInput): Promise<Order[]> {
      const parsed = parseInput(input);
      const now = ctx.now();
      const created: Order[] = [];

      if (parsed.kind === 'oco') {
        const legs = parsed.legs as [ParsedLeg, ParsedLeg];
        checkLeg(ctx, parsed, now);
        for (const leg of legs) checkLeg(ctx, leg, now);

        const parentId = newId('ord');
        const parent: Order = {
          id: parentId,
          kind: 'oco',
          side: parsed.side,
          tokenIn: parsed.tokenIn,
          tokenOut: parsed.tokenOut,
          amountIn: parsed.amountIn,
          trigger: {},
          bounds: resolveBounds(ctx.config.bounds, parsed.bounds),
          live: parsed.live || legs.some((leg) => leg.live),
          status: 'open',
          createdAt: now,
          expiresAt: parsed.expiresAt,
          parentId: null,
          reason: 'Waiting on either leg.',
        };
        ctx.store.insertOrder(parent);
        writeCreated(ctx, parent);
        created.push(parent);

        for (const leg of legs) {
          const child = buildOrder(ctx, leg, {
            id: newId('ord'),
            kind: legKind(leg),
            now,
            parentId,
          });
          ctx.store.insertOrder(child);
          writeCreated(ctx, child);
          created.push(child);
        }
        return created;
      }

      checkLeg(ctx, parsed, now);
      const order = buildOrder(ctx, parsed, { id: newId('ord'), kind: parsed.kind, now, parentId: null });
      ctx.store.insertOrder(order);
      writeCreated(ctx, order);
      return [order];
    },

    get(id: string): Order | undefined {
      return ctx.store.getOrder(id);
    },

    list(filter): Order[] {
      return ctx.store.listOrders(filter);
    },

    async cancel(id: string, reason?: string): Promise<Order> {
      const order = ctx.store.getOrder(id);
      if (!order) throw new NotFoundError(`Order ${id}`, { id });
      const why = reason ?? 'Cancelled by request.';
      if (order.status === 'cancelled') return order;
      if (!isActive(order.status)) {
        throw new InvalidStateError(`Order ${id} is ${order.status} and cannot be cancelled.`, {
          id,
          status: order.status,
        });
      }

      if (order.kind === 'oco' && order.parentId === null) {
        for (const child of ctx.store.listOrders({ parentId: id })) {
          if (isActive(child.status)) transition(ctx, child, 'cancelled', why);
        }
        return transition(ctx, order, 'cancelled', why);
      }

      const cancelled = transition(ctx, order, 'cancelled', why);
      if (order.parentId) {
        cancelSiblings(ctx, order, 'The other leg of the pair was cancelled.');
        settleParent(ctx, order.parentId);
      }
      return cancelled;
    },
  };
}

/** Validate and normalise caller input. Throws {@link OrderInvalidError} with the field named. */
export function parseInput(input: CreateOrderInput): ParsedCreateOrderInput {
  const result = CreateOrderInputSchema.safeParse(input);
  if (result.success) return result.data;
  const first = result.error.issues[0];
  const where = first && first.path.length > 0 ? first.path.join('.') : 'order';
  throw new OrderInvalidError(`${where} is invalid: ${first?.message ?? 'unknown reason'}`, {
    issues: result.error.issues,
  });
}

/** Cancel every other leg of the same OCO pair. */
export function cancelSiblings(ctx: OrdersContext, order: Order, reason: string): Order[] {
  if (!order.parentId) return [];
  const cancelled: Order[] = [];
  for (const sibling of ctx.store.listOrders({ parentId: order.parentId })) {
    if (sibling.id === order.id || !isActive(sibling.status)) continue;
    cancelled.push(transition(ctx, sibling, 'cancelled', reason));
  }
  return cancelled;
}

/** Close an OCO parent once no leg can still change. */
export function settleParent(ctx: OrdersContext, parentId: string): Order | undefined {
  const parent = ctx.store.getOrder(parentId);
  if (!parent || !isActive(parent.status)) return parent;
  const children = ctx.store.listOrders({ parentId });
  if (children.length === 0 || children.some((child) => isActive(child.status))) return parent;

  const statuses = new Set<OrderStatus>(children.map((child) => child.status));
  if (statuses.has('filled')) return transition(ctx, parent, 'filled', 'One leg filled and the other was cancelled.');
  if (statuses.has('failed')) return transition(ctx, parent, 'failed', 'Both legs stopped without a fill.');
  if (statuses.has('expired')) return transition(ctx, parent, 'expired', 'Both legs expired.');
  return transition(ctx, parent, 'cancelled', 'Both legs were cancelled.');
}

function buildOrder(
  ctx: OrdersContext,
  leg: ParsedLeg,
  meta: { id: string; kind: Order['kind']; now: number; parentId: string | null },
): Order {
  return {
    id: meta.id,
    kind: meta.kind,
    side: leg.side,
    tokenIn: leg.tokenIn as Address,
    tokenOut: leg.tokenOut as Address,
    amountIn: leg.amountIn,
    trigger: leg.trigger,
    bounds: resolveBounds(ctx.config.bounds, leg.bounds),
    live: leg.live,
    status: 'open',
    createdAt: meta.now,
    expiresAt: leg.expiresAt,
    parentId: meta.parentId,
    reason: 'Waiting on the trigger.',
  };
}

/** The kind a bare OCO leg is stored under, read off its own conditions. */
function legKind(leg: ParsedLeg): Order['kind'] {
  if (leg.trigger.premiumLteBps !== undefined || leg.trigger.premiumGteBps !== undefined) return 'premium';
  if (leg.trigger.at !== undefined || leg.trigger.atNextOpenOffsetSec !== undefined) return 'atOpen';
  if (leg.side === 'sell' && leg.trigger.priceLte !== undefined && leg.trigger.priceGte === undefined) return 'stop';
  if (leg.side === 'sell' && leg.trigger.priceGte !== undefined) return 'takeProfit';
  return 'limit';
}

/** Refusals that need the config and the clock: live permission, bound ceilings, times in the past. */
function checkLeg(ctx: OrdersContext, leg: ParsedLeg, now: number): void {
  if (leg.live) {
    const blocked = liveBlockedReason(ctx.config);
    if (blocked) throw new LiveDisabledError(blocked);
  }

  const bounds = resolveBounds(ctx.config.bounds, leg.bounds);
  checkCeiling(bounds, ctx.config.bounds);

  if (leg.expiresAt !== null && leg.expiresAt <= now) {
    throw new OrderInvalidError(
      `expiresAt is ${new Date(leg.expiresAt).toISOString()}, which has already passed.`,
      { expiresAt: leg.expiresAt, now },
    );
  }
  if (leg.trigger.at !== undefined && leg.trigger.at <= now) {
    throw new OrderInvalidError(
      `trigger.at is ${new Date(leg.trigger.at).toISOString()}, which has already passed.`,
      { at: leg.trigger.at, now },
    );
  }
  if (leg.expiresAt !== null && leg.trigger.at !== undefined && leg.trigger.at > leg.expiresAt) {
    throw new OrderInvalidError(
      `trigger.at is after expiresAt, so the order would expire ${Math.round((leg.trigger.at - leg.expiresAt) / 1000)} seconds before it could fire.`,
      { at: leg.trigger.at, expiresAt: leg.expiresAt },
    );
  }
}

/** An order may tighten a configured bound. It may never widen one. */
function checkCeiling(bounds: OrderBounds, configured: OrderBounds): void {
  if (bounds.maxOrderNotionalUsd > configured.maxOrderNotionalUsd) {
    throw new BoundsExceededError(
      'maxOrderNotionalUsd',
      configured.maxOrderNotionalUsd,
      bounds.maxOrderNotionalUsd,
      'USD',
    );
  }
  if (bounds.maxSlippageBps > configured.maxSlippageBps) {
    throw new BoundsExceededError('maxSlippageBps', configured.maxSlippageBps, bounds.maxSlippageBps, 'bps');
  }
  if (bounds.maxBuyPremiumBps > configured.maxBuyPremiumBps) {
    throw new BoundsExceededError('maxBuyPremiumBps', configured.maxBuyPremiumBps, bounds.maxBuyPremiumBps, 'bps');
  }
}

function writeCreated(ctx: OrdersContext, order: Order): void {
  ctx.store.appendEvent({
    ts: order.createdAt,
    kind: 'order_created',
    subject: order.id,
    detail: {
      kind: order.kind,
      side: order.side,
      tokenIn: order.tokenIn,
      tokenOut: order.tokenOut,
      amountIn: order.amountIn.toString(),
      trigger: order.trigger,
      bounds: order.bounds,
      live: order.live,
      parentId: order.parentId,
      expiresAt: order.expiresAt,
    },
  });
  ctx.logger.info('order created', {
    order: order.id,
    kind: order.kind,
    side: order.side,
    live: order.live,
    parentId: order.parentId ?? undefined,
  });
}
