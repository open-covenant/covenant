/**
 * Keeping the short where the position needs it.
 *
 * The desk holds one target per symbol: the dollar value of the stock leg it is
 * cancelling. On every pass it compares that target against the short actually
 * held on the venue and acts when the gap is wider than the configured drift,
 * or when funding has turned against the short by more than the configured
 * limit. Everything it decides is written to the event log, whether or not an
 * order goes out.
 */

import type { Config } from '../core/config.js';
import { isDeskError } from '../core/errors.js';
import type { Logger } from '../core/logger.js';
import type { Store } from '../core/store.js';
import { quantity, type HedgePlan, type HedgePosition } from '../core/types.js';
import type { LighterRhClient } from './lighter-rh.js';
import type { HedgeSizer } from './sizer.js';
import type { Rebalancer } from './index.js';

/** A stock leg the desk is holding a short against. */
export interface HedgeTarget {
  readonly symbol: string;
  /** Dollar value of the stock exposure being cancelled. */
  readonly stockLegUsd: number;
  readonly updatedAt: number;
}

/** The rebalancer plus the target book it works from. */
export interface DeskRebalancer extends Rebalancer {
  /** Track a stock leg, or update the size of one already tracked. */
  setTarget(symbol: string, stockLegUsd: number): HedgeTarget;
  /** Stop tracking a stock leg. Leaves any open short in place. */
  clearTarget(symbol: string): void;
  /** Every stock leg currently tracked. */
  targets(): HedgeTarget[];
  /** Why a plan would or would not be acted on, without acting on it. */
  shouldAct(plan: HedgePlan): { act: boolean; reason: string };
}

export interface RebalancerOptions {
  readonly config: Config;
  readonly logger: Logger;
  readonly store: Store;
  readonly client: LighterRhClient;
  readonly sizer: HedgeSizer;
  readonly now?: () => number;
}

/** Drift and funding driven maintenance of the short leg. */
export class HedgeRebalancer implements DeskRebalancer {
  readonly #config: Config;
  readonly #logger: Logger;
  readonly #store: Store;
  readonly #client: LighterRhClient;
  readonly #sizer: HedgeSizer;
  readonly #now: () => number;
  readonly #targets = new Map<string, HedgeTarget>();

  #timer: NodeJS.Timeout | undefined;
  #inFlight = false;

  constructor(options: RebalancerOptions) {
    this.#config = options.config;
    this.#logger = options.logger.child({ component: 'hedge.rebalancer' });
    this.#store = options.store;
    this.#client = options.client;
    this.#sizer = options.sizer;
    this.#now = options.now ?? Date.now;
  }

  /** Start the loop. Calling it twice leaves one loop running. */
  start(): void {
    if (this.#timer !== undefined) return;
    const intervalMs = this.#config.hedge.intervalSec * 1000;
    this.#timer = setInterval(() => {
      void this.#safeTick();
    }, intervalMs);
    this.#timer.unref?.();
    this.#logger.info('hedge loop started', { intervalSec: this.#config.hedge.intervalSec });
  }

  stop(): void {
    if (this.#timer === undefined) return;
    clearInterval(this.#timer);
    this.#timer = undefined;
    this.#logger.info('hedge loop stopped');
  }

  running(): boolean {
    return this.#timer !== undefined;
  }

  setTarget(symbol: string, stockLegUsd: number): HedgeTarget {
    const target: HedgeTarget = {
      symbol: symbol.trim().toUpperCase(),
      stockLegUsd,
      updatedAt: this.#now(),
    };
    this.#targets.set(target.symbol, target);
    return target;
  }

  clearTarget(symbol: string): void {
    this.#targets.delete(symbol.trim().toUpperCase());
  }

  targets(): HedgeTarget[] {
    return [...this.#targets.values()].sort((left, right) => left.symbol.localeCompare(right.symbol));
  }

  /**
   * Act when the short has drifted past the configured band, or when funding
   * has turned against it by more than the configured limit.
   *
   * Funding is positive when longs pay shorts, so a short is paid to hold the
   * position. A rate below the negative limit means the short is paying, which
   * is the case the desk closes.
   */
  shouldAct(plan: HedgePlan): { act: boolean; reason: string } {
    const driftBps = plan.driftBps?.value ?? 0;
    const driftLimit = this.#config.hedge.driftBps;
    const fundingBps = plan.fundingBps8h?.value;
    const fundingLimit = this.#config.hedge.maxFundingBps8h;
    const holdsShort = Math.max(0, -(plan.currentShortBase?.value ?? 0)) > 0;

    if (holdsShort && fundingBps !== undefined && fundingBps < -fundingLimit) {
      return {
        act: true,
        reason: `Funding on ${plan.symbol} is ${fundingBps.toFixed(2)} bps over eight hours against the short, past the ${fundingLimit} bps limit.`,
      };
    }
    if (plan.action === 'none') {
      return { act: false, reason: `The ${plan.symbol} short is within ${driftLimit} bps of target.` };
    }
    if (Math.abs(driftBps) > driftLimit) {
      return {
        act: true,
        reason: `The ${plan.symbol} short is ${Math.round(driftBps)} bps from target, past the ${driftLimit} bps band.`,
      };
    }
    return { act: false, reason: `The ${plan.symbol} short is within ${driftLimit} bps of target.` };
  }

  /** One pass over every tracked stock leg. Returns the plans it acted on. */
  async tick(): Promise<HedgePlan[]> {
    await this.#recordPositions();

    const acted: HedgePlan[] = [];
    for (const target of this.targets()) {
      let plan: HedgePlan;
      try {
        plan = await this.#sizer.plan({ symbol: target.symbol, stockLegUsd: target.stockLegUsd });
      } catch (error) {
        this.#note('hedge.plan_failed', target.symbol, {
          reason: isDeskError(error) ? error.reason : (error as Error).message,
        });
        continue;
      }

      const decision = this.shouldAct(plan);
      if (!decision.act) {
        this.#logger.debug('hedge holds', { symbol: plan.symbol, reason: decision.reason });
        continue;
      }

      const intended = this.#forDecision(plan, decision.reason);
      const applied = await this.apply(intended);
      acted.push(applied);
    }
    return acted;
  }

  /**
   * Send a plan.
   *
   * A plan that has nothing to send, that the desk cannot sign for, or whose
   * size the venue would not accept comes back with `executable` false and the
   * reason it stopped. The size that goes out is the gap truncated to the
   * market's step, so a hedge is never larger than the exposure it cancels.
   */
  async apply(plan: HedgePlan): Promise<HedgePlan> {
    const size = plan.actionSizeBase?.value ?? 0;
    if (plan.action === 'none' || size <= 0) {
      const reason = plan.reason ?? `There is nothing to send for ${plan.symbol}.`;
      this.#note('hedge.blocked', plan.symbol, { action: plan.action, reason });
      return { ...plan, executable: false, reason };
    }
    if (plan.executable === false && plan.reason !== undefined) {
      this.#note('hedge.blocked', plan.symbol, { action: plan.action, sizeBase: size, reason: plan.reason });
      return { ...plan, executable: false, reason: plan.reason };
    }

    const ready = await this.#client.canTrade();
    if (!ready.ok) {
      const reason = ready.reason ?? 'Signed orders are not available.';
      this.#note('hedge.blocked', plan.symbol, { action: plan.action, sizeBase: size, reason });
      return { ...plan, executable: false, reason };
    }

    const reduceOnly = plan.action === 'decreaseShort' || plan.action === 'unwind';
    const marketId = plan.marketId;
    if (marketId === undefined) {
      const reason = `No Lighter Robinhood Chain market is recorded for ${plan.symbol}.`;
      this.#note('hedge.blocked', plan.symbol, { reason });
      return { ...plan, executable: false, reason };
    }

    let sizeBase: number;
    try {
      const market = await this.#client.market(marketId);
      sizeBase = this.#sizer.roundSize(size, market);
      if (sizeBase <= 0 || sizeBase > size) {
        const reason = `Closing the ${plan.symbol} gap needs a ${size.toFixed(market.sizeDecimals)} order and the smallest ${plan.symbol} order the venue accepts is ${market.minBaseAmount.toFixed(market.sizeDecimals)}.`;
        this.#note('hedge.blocked', plan.symbol, { action: plan.action, sizeBase: size, reason });
        return { ...plan, executable: false, reason };
      }
    } catch (error) {
      const reason = isDeskError(error) ? error.reason : (error as Error).message;
      this.#note('hedge.blocked', plan.symbol, { action: plan.action, sizeBase: size, reason });
      return { ...plan, executable: false, reason };
    }

    const result = await this.#client.placeOrder({
      marketId,
      side: reduceOnly ? 'buy' : 'sell',
      sizeBase,
      reduceOnly,
      maxNotionalUsd: this.#config.hedge.maxNotionalUsd,
    });

    if (!result.sent) {
      const reason = result.reason ?? 'The venue did not accept the order.';
      this.#note('hedge.blocked', plan.symbol, { action: plan.action, sizeBase, reason });
      return { ...plan, executable: false, reason };
    }

    this.#note('hedge.sent', plan.symbol, {
      action: plan.action,
      side: reduceOnly ? 'buy' : 'sell',
      sizeBase,
      orderId: result.orderId,
    });
    return { ...plan, executable: true };
  }

  /** Close the short on one symbol, or on every symbol when none is named. */
  async unwind(symbol?: string): Promise<HedgePlan[]> {
    const wanted = symbol?.trim().toUpperCase();
    const positions = await this.#client.positions();
    const shorts = positions.filter(
      (position) =>
        position.sizeBase.value < 0 && (wanted === undefined || position.symbol.toUpperCase() === wanted),
    );

    const out: HedgePlan[] = [];
    for (const position of shorts) {
      const applied = await this.apply(this.#unwindPlan(position));
      this.clearTarget(position.symbol);
      out.push(applied);
    }
    return out;
  }

  /** Turn a live short into a plan that closes it. */
  #unwindPlan(position: HedgePosition): HedgePlan {
    const asOf = this.#now();
    const size = Math.abs(position.sizeBase.value);
    return {
      symbol: position.symbol.toUpperCase(),
      marketId: position.marketId,
      stockLegNotionalUsd: quantity(0, 'USD', 'derived', asOf),
      referencePrice: position.markPrice,
      targetShortBase: quantity(0, 'token', 'derived', asOf),
      currentShortBase: position.sizeBase,
      driftBps: quantity(-10_000, 'bps', 'derived', asOf),
      action: 'unwind',
      actionSizeBase: quantity(size, 'token', 'derived', asOf),
      ...(position.fundingBps8h === undefined ? {} : { fundingBps8h: position.fundingBps8h }),
      executable: true,
      asOf,
    };
  }

  /** Funding that has turned against a short is closed rather than resized. */
  #forDecision(plan: HedgePlan, reason: string): HedgePlan {
    const fundingBps = plan.fundingBps8h?.value;
    const currentShort = Math.max(0, -(plan.currentShortBase?.value ?? 0));
    const fundingAgainst =
      currentShort > 0 && fundingBps !== undefined && fundingBps < -this.#config.hedge.maxFundingBps8h;
    if (!fundingAgainst) {
      this.#note('hedge.plan', plan.symbol, { action: plan.action, reason });
      return plan;
    }
    this.#note('hedge.funding_flip', plan.symbol, { fundingBps8h: fundingBps, reason });
    // The sizer wrote its own reason for the plan it made, which was a resize.
    // Closing the position is a different decision, so that reason goes.
    const { reason: _replaced, ...rest } = plan;
    return {
      ...rest,
      action: 'unwind',
      actionSizeBase: quantity(currentShort, 'token', 'derived', this.#now()),
      executable: true,
    };
  }

  /** Mirror the venue's positions into the store so surfaces can read them. */
  async #recordPositions(): Promise<void> {
    try {
      const positions = await this.#client.positions();
      for (const position of positions) this.#store.upsertHedge(position);
    } catch (error) {
      this.#note('hedge.positions_failed', undefined, {
        reason: isDeskError(error) ? error.reason : (error as Error).message,
      });
    }
  }

  async #safeTick(): Promise<void> {
    if (this.#inFlight) return;
    this.#inFlight = true;
    try {
      await this.tick();
    } catch (error) {
      this.#logger.error('hedge pass failed', {
        reason: isDeskError(error) ? error.reason : (error as Error).message,
      });
    } finally {
      this.#inFlight = false;
    }
  }

  #note(kind: string, subject: string | undefined, detail: Record<string, unknown>): void {
    this.#store.appendEvent({
      ts: this.#now(),
      kind,
      ...(subject === undefined ? {} : { subject }),
      detail,
    });
    this.#logger.info(kind, { subject, ...detail });
  }
}
