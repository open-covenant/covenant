/**
 * The trigger engine.
 *
 * Every `config.engineIntervalSec` (five seconds by default) the engine reads
 * the open orders, expires the ones that ran out of time, tests the rest
 * against the latest prices, and executes what fired. A dry run quotes,
 * applies the same bounds, and records the fill it would have produced. A
 * signed swap needs three things at once: the desk is live, the order asked to
 * be live, and the jurisdiction notice has been acknowledged.
 */

import { liveBlockedReason } from '../core/config.js';
import {
  BoundsExceededError,
  DeskError,
  InvalidStateError,
  LiveDisabledError,
  UpstreamError,
} from '../core/errors.js';
import { toDecimalNumber, type Execution, type Order, type OrderTrigger, type Quantity } from '../core/types.js';
import type { QuoteResult } from '../chain/index.js';
import { cancelSiblings, settleParent } from './book.js';
import { countConditions } from './schema.js';
import {
  DAY_MS,
  bps,
  decimalsFor,
  isActive,
  newId,
  note,
  subjectToken,
  transition,
  usd,
  type OrdersContext,
} from './support.js';
import type { BoundsCheck, Engine, OrderSnapshot, TriggerContext } from './types.js';

/** Orders read per pass. Above this the engine is behind and the log says so. */
const MAX_ORDERS_PER_TICK = 1000;

/** Deadline handed to the router on a signed swap, seconds from now. */
const SWAP_DEADLINE_SEC = 120;

export function createEngine(ctx: OrdersContext): Engine {
  let timer: NodeJS.Timeout | undefined;
  let ticking = false;

  const engine: Engine = {
    start() {
      if (timer) return;
      const intervalMs = ctx.config.engineIntervalSec * 1000;
      timer = setInterval(() => {
        if (ticking) return;
        ticking = true;
        void engine
          .tick()
          .catch((error: unknown) => {
            ctx.logger.error('order pass failed', { reason: message(error) });
          })
          .finally(() => {
            ticking = false;
          });
      }, intervalMs);
      timer.unref?.();
      ctx.logger.info('order engine started', { intervalSec: ctx.config.engineIntervalSec });
    },

    stop() {
      if (!timer) return;
      clearInterval(timer);
      timer = undefined;
      ctx.logger.info('order engine stopped');
    },

    running() {
      return timer !== undefined;
    },

    async tick(at?: number): Promise<Order[]> {
      const now = at ?? ctx.now();
      const fired: Order[] = [];
      const open = ctx.store.listOrders({ status: 'open', limit: MAX_ORDERS_PER_TICK });
      if (open.length === MAX_ORDERS_PER_TICK) {
        ctx.logger.warn('order pass hit its read limit', { limit: MAX_ORDERS_PER_TICK });
      }

      for (const listed of open) {
        // A leg that filled earlier in this pass may already have cancelled
        // this one, so read the status again before acting on it.
        const order = ctx.store.getOrder(listed.id) ?? listed;
        if (order.status !== 'open' || isContainer(order)) continue;

        if (order.expiresAt !== null && order.expiresAt <= now) {
          transition(ctx, order, 'expired', `Expired at ${new Date(order.expiresAt).toISOString()}.`);
          if (order.parentId) settleParent(ctx, order.parentId);
          continue;
        }

        let context: TriggerContext;
        try {
          context = await contextFor(order, now);
        } catch (error) {
          ctx.logger.warn('order could not be priced', { order: order.id, reason: message(error) });
          continue;
        }
        if (!engine.isTriggered(order.trigger, context)) continue;

        const triggered = transition(ctx, order, 'triggered', describeTrigger(order.trigger, context), {
          usdOnchain: context.usdOnchain,
          usdFair: context.usdFair,
          premiumBps: context.premiumBps,
        });
        try {
          await engine.execute(triggered);
        } catch (error) {
          ctx.logger.warn('order did not execute', { order: order.id, reason: message(error) });
        }
        fired.push(ctx.store.getOrder(order.id) ?? triggered);
      }

      return fired;
    },

    isTriggered(trigger: OrderTrigger, context: TriggerContext): boolean {
      if (countConditions(trigger) === 0) return false;

      const price = (trigger.priceBasis ?? 'usdOnchain') === 'usdFair' ? context.usdFair : context.usdOnchain;
      if (trigger.priceLte !== undefined && !(price !== undefined && price <= trigger.priceLte)) return false;
      if (trigger.priceGte !== undefined && !(price !== undefined && price >= trigger.priceGte)) return false;

      const premium = context.premiumBps;
      if (trigger.premiumLteBps !== undefined && !(premium !== undefined && premium <= trigger.premiumLteBps)) {
        return false;
      }
      if (trigger.premiumGteBps !== undefined && !(premium !== undefined && premium >= trigger.premiumGteBps)) {
        return false;
      }

      if (trigger.at !== undefined && context.now < trigger.at) return false;
      if (trigger.atNextOpenOffsetSec !== undefined) {
        if (context.nextOpen === undefined) return false;
        if (context.now < context.nextOpen + trigger.atNextOpenOffsetSec * 1000) return false;
      }
      return true;
    },

    checkBounds(order: Order, quotedUsd: number, premiumBps: number | undefined): BoundsCheck {
      const configured = ctx.config.bounds;
      const bounds = order.bounds;

      if (bounds.maxSlippageBps > configured.maxSlippageBps) {
        return refusal(
          'maxSlippageBps',
          configured.maxSlippageBps,
          bounds.maxSlippageBps,
          'bps',
          `The configured slippage cap is ${bps(configured.maxSlippageBps)} bps and this order asks for ${bps(bounds.maxSlippageBps)} bps.`,
        );
      }
      if (bounds.maxOrderNotionalUsd > configured.maxOrderNotionalUsd) {
        return refusal(
          'maxOrderNotionalUsd',
          configured.maxOrderNotionalUsd,
          bounds.maxOrderNotionalUsd,
          'USD',
          `The configured order cap is ${usd(configured.maxOrderNotionalUsd)} USD and this order asks for ${usd(bounds.maxOrderNotionalUsd)} USD.`,
        );
      }
      if (bounds.maxBuyPremiumBps > configured.maxBuyPremiumBps) {
        return refusal(
          'maxBuyPremiumBps',
          configured.maxBuyPremiumBps,
          bounds.maxBuyPremiumBps,
          'bps',
          `The configured buy premium cap is ${bps(configured.maxBuyPremiumBps)} bps and this order asks for ${bps(bounds.maxBuyPremiumBps)} bps.`,
        );
      }

      if (!Number.isFinite(quotedUsd) || quotedUsd <= 0) {
        return {
          ok: false,
          bound: 'notionalUsd',
          reason:
            'The size of this order in USD could not be measured, so the notional caps cannot be applied. No reference price was available for the token being traded.',
        };
      }

      if (quotedUsd > bounds.maxOrderNotionalUsd) {
        return refusal(
          'maxOrderNotionalUsd',
          bounds.maxOrderNotionalUsd,
          quotedUsd,
          'USD',
          `The order cap is ${usd(bounds.maxOrderNotionalUsd)} USD and this fill is ${usd(quotedUsd)} USD.`,
        );
      }

      const used = engine.dailyNotionalUsd();
      const total = used + quotedUsd;
      if (total > configured.maxDailyNotionalUsd) {
        return refusal(
          'maxDailyNotionalUsd',
          configured.maxDailyNotionalUsd,
          total,
          'USD',
          `The daily cap is ${usd(configured.maxDailyNotionalUsd)} USD, ${usd(used)} USD has been used in the last 24 hours, and this fill would take the total to ${usd(total)} USD.`,
        );
      }

      if (order.side === 'buy') {
        if (premiumBps === undefined) {
          return {
            ok: false,
            bound: 'maxBuyPremiumBps',
            reason:
              'The stock leg premium could not be measured, so the buy premium cap cannot be applied. No reference price was available for the stock this token is quoted in.',
          };
        }
        if (premiumBps > bounds.maxBuyPremiumBps) {
          return refusal(
            'maxBuyPremiumBps',
            bounds.maxBuyPremiumBps,
            premiumBps,
            'bps',
            `The buy premium cap is ${bps(bounds.maxBuyPremiumBps)} bps and the stock leg is at ${bps(premiumBps)} bps.`,
          );
        }
      }

      return { ok: true };
    },

    dailyNotionalUsd(at?: number): number {
      // A dry run counts against the cap too, so the limit is visible before
      // the desk ever signs anything. A reverted fill moved nothing and does not.
      return ctx.store.filledNotionalUsdSince((at ?? ctx.now()) - DAY_MS, [
        'simulated',
        'sent',
        'confirmed',
      ]);
    },

    async execute(order: Order): Promise<Execution> {
      if (!isActive(order.status)) {
        throw new InvalidStateError(`Order ${order.id} is ${order.status} and cannot be executed.`, {
          id: order.id,
          status: order.status,
        });
      }
      if (isContainer(order)) {
        throw new InvalidStateError(`Order ${order.id} is an OCO pair. Its legs execute, the pair does not.`, {
          id: order.id,
        });
      }

      const now = ctx.now();
      const blocked = liveBlockedReason(ctx.config);
      if (order.live && blocked) {
        fail(order, blocked);
        throw new LiveDisabledError(blocked);
      }
      const live = order.live && blocked === null;

      let quote: QuoteResult;
      try {
        quote = await ctx.chain.quoter.quoteExactInput({
          tokenIn: order.tokenIn,
          tokenOut: order.tokenOut,
          amountIn: order.amountIn,
        });
      } catch (error) {
        const reason = `The quoter did not return a price: ${message(error)}`;
        fail(order, reason);
        throw new UpstreamError('V4Quoter', message(error), { order: order.id });
      }
      if (quote.amountOut <= 0n) {
        const reason = 'The quoter returned nothing for this size, so there is no price to fill at.';
        fail(order, reason);
        throw new UpstreamError('V4Quoter', reason, { order: order.id });
      }

      const snapshot = await safeSnapshot(order, now);
      const subject = subjectToken(order);
      const decimals = await decimalsFor(ctx, subject);
      if (decimals === undefined) {
        const reason = `The decimals of ${subject} could not be read, so the size of this fill in USD cannot be measured and the notional caps cannot be applied.`;
        note(ctx, 'order_refused', order.id, { bound: 'notionalUsd', token: subject, reason });
        fail(order, reason);
        throw new DeskError('bounds_exceeded', reason, { order: order.id, token: subject });
      }
      const quotedUsd = notionalUsd(order, quote, snapshot, decimals);
      const check = engine.checkBounds(order, quotedUsd, snapshot.premiumBps);
      if (!check.ok) {
        const reason = check.reason ?? 'A safety bound refused this order.';
        note(ctx, 'order_refused', order.id, {
          bound: check.bound,
          limit: check.limit,
          actual: check.actual,
          unit: check.unit,
          reason,
        });
        fail(order, reason);
        throw check.bound !== undefined && check.limit !== undefined && check.actual !== undefined
          ? new BoundsExceededError(check.bound, check.limit, check.actual, check.unit ?? 'USD')
          : new DeskError('bounds_exceeded', reason, { order: order.id, bound: check.bound });
      }

      const minAmountOut = (quote.amountOut * BigInt(10_000 - order.bounds.maxSlippageBps)) / 10_000n;
      let amountOut = quote.amountOut;
      let effectivePrice: Quantity = quote.effectivePrice;
      let txHash: Execution['txHash'];
      let gasUsed: bigint | undefined;
      let blockNumber: bigint | undefined = quote.blockNumber;
      let status: Execution['status'] = 'simulated';

      if (live) {
        try {
          const result = await ctx.chain.swap.execute({
            tokenIn: order.tokenIn,
            tokenOut: order.tokenOut,
            amountIn: order.amountIn,
            // The whole quoted route, so a two-hop fill goes through the pools
            // it was priced on rather than through the first one of them.
            route: quote.route,
            ...(quote.route.length === 1 && quote.route[0] ? { poolId: quote.route[0] } : {}),
            minAmountOut,
            deadlineSec: SWAP_DEADLINE_SEC,
            live: true,
          });
          amountOut = result.amountOut;
          effectivePrice = result.effectivePrice;
          txHash = result.txHash;
          gasUsed = result.gasUsed;
          blockNumber = result.blockNumber ?? blockNumber;
          status = result.blockNumber === undefined ? 'sent' : 'confirmed';
        } catch (error) {
          const reason = `The swap did not go through: ${message(error)}`;
          fail(order, reason);
          throw new UpstreamError('UniversalRouter', message(error), { order: order.id });
        }
      }

      const slippageBps = realizedSlippageBps(quote.amountOut, amountOut);
      if (slippageBps > order.bounds.maxSlippageBps) {
        ctx.logger.warn('fill came in below the slippage bound', {
          order: order.id,
          bound: order.bounds.maxSlippageBps,
          realized: slippageBps,
        });
      }

      const execution: Execution = {
        id: newId('exe'),
        orderId: order.id,
        live,
        txHash,
        amountIn: order.amountIn,
        amountOut,
        effectivePrice,
        quotedAmountOut: quote.amountOut,
        notionalUsd: { value: quotedUsd, unit: 'USD', source: 'derived', asOf: now },
        slippageBps,
        gasUsed,
        blockNumber,
        status,
        reason: live ? undefined : 'Dry run. No transaction was signed.',
        createdAt: now,
      };
      ctx.store.insertExecution(execution);

      transition(
        ctx,
        order,
        'filled',
        live
          ? `Filled for ${usd(quotedUsd)} USD.`
          : `Dry run recorded a fill of ${usd(quotedUsd)} USD at the quoted price.`,
        { execution: execution.id, live, txHash, notionalUsd: quotedUsd, slippageBps },
      );

      if (order.parentId) {
        cancelSiblings(ctx, order, 'The other leg of the pair filled first.');
        settleParent(ctx, order.parentId);
      }

      return execution;
    },
  };

  function fail(order: Order, reason: string): void {
    transition(ctx, order, 'failed', reason);
    if (order.parentId) settleParent(ctx, order.parentId);
  }

  async function safeSnapshot(order: Order, now: number): Promise<OrderSnapshot> {
    try {
      return await ctx.snapshots.snapshot(order, now);
    } catch (error) {
      const reason = message(error);
      ctx.logger.debug('no price for the order subject', { order: order.id, reason });
      return { token: subjectToken(order), asOf: now, note: reason };
    }
  }

  async function contextFor(order: Order, now: number): Promise<TriggerContext> {
    const needsPrice =
      order.trigger.priceLte !== undefined ||
      order.trigger.priceGte !== undefined ||
      order.trigger.premiumLteBps !== undefined ||
      order.trigger.premiumGteBps !== undefined;

    const snapshot = needsPrice ? await safeSnapshot(order, now) : undefined;

    let nextOpen: number | undefined;
    if (order.trigger.atNextOpenOffsetSec !== undefined) {
      try {
        nextOpen = ctx.fairvalue.session.nextOpen(order.createdAt);
      } catch (error) {
        ctx.logger.debug('the calendar did not answer', { order: order.id, reason: message(error) });
      }
    }

    return {
      usdOnchain: snapshot?.usdOnchain,
      usdFair: snapshot?.usdFair,
      premiumBps: snapshot?.premiumBps,
      nextOpen,
      now,
    };
  }

  /**
   * Size of the fill in USD, measured on the token the trader cares about:
   * what a buy receives, what a sell gives up.
   */
  function notionalUsd(
    order: Order,
    quote: QuoteResult,
    snapshot: OrderSnapshot,
    decimals: number,
  ): number {
    const price = snapshot.usdOnchain ?? snapshot.usdFair;
    if (price === undefined || !Number.isFinite(price) || price <= 0) return Number.NaN;
    const quantity = toDecimalNumber(order.side === 'buy' ? quote.amountOut : order.amountIn, decimals);
    return price * quantity;
  }

  return engine;
}

/** True for the OCO parent, which holds two legs and never executes itself. */
export function isContainer(order: Order): boolean {
  return order.kind === 'oco' && order.parentId === null;
}

function refusal(bound: string, limit: number, actual: number, unit: string, reason: string): BoundsCheck {
  return { ok: false, bound, limit, actual, unit, reason };
}

/** How far the fill came in under the quote, basis points. Never negative. */
export function realizedSlippageBps(quoted: bigint, actual: bigint): number {
  if (quoted <= 0n || actual >= quoted) return 0;
  return Number(((quoted - actual) * 10_000n) / quoted);
}

function describeTrigger(trigger: OrderTrigger, context: TriggerContext): string {
  const basis = (trigger.priceBasis ?? 'usdOnchain') === 'usdFair' ? 'fair value' : 'the on-chain price';
  const price = (trigger.priceBasis ?? 'usdOnchain') === 'usdFair' ? context.usdFair : context.usdOnchain;
  const parts: string[] = [];
  if (trigger.priceLte !== undefined && price !== undefined) {
    parts.push(`${basis} is ${usd(price)} USD, at or below ${usd(trigger.priceLte)} USD`);
  }
  if (trigger.priceGte !== undefined && price !== undefined) {
    parts.push(`${basis} is ${usd(price)} USD, at or above ${usd(trigger.priceGte)} USD`);
  }
  if (trigger.premiumLteBps !== undefined && context.premiumBps !== undefined) {
    parts.push(`the stock leg premium is ${bps(context.premiumBps)} bps, at or below ${bps(trigger.premiumLteBps)} bps`);
  }
  if (trigger.premiumGteBps !== undefined && context.premiumBps !== undefined) {
    parts.push(`the stock leg premium is ${bps(context.premiumBps)} bps, at or above ${bps(trigger.premiumGteBps)} bps`);
  }
  if (trigger.at !== undefined) parts.push(`the time reached ${new Date(trigger.at).toISOString()}`);
  if (trigger.atNextOpenOffsetSec !== undefined && context.nextOpen !== undefined) {
    const target = context.nextOpen + trigger.atNextOpenOffsetSec * 1000;
    parts.push(`the market opened and ${trigger.atNextOpenOffsetSec} seconds passed, at ${new Date(target).toISOString()}`);
  }
  return parts.length === 0 ? 'The conditions were met.' : `Triggered because ${parts.join(' and ')}.`;
}

function message(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}
