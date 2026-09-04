/**
 * Turning a stock leg into a perpetual short.
 *
 * A memecoin quoted in a stock token carries that stock inside it. The size of
 * the stock leg is what the holder is exposed to, and the short that cancels it
 * is that notional divided by the perpetual's mark price, rounded to what the
 * market can express.
 */

import type { Config } from '../core/config.js';
import { NoReferenceError, NotFoundError } from '../core/errors.js';
import type { Logger } from '../core/logger.js';
import { quantity, type HedgePlan, type Quantity } from '../core/types.js';
import type { FairValueModule } from '../fairvalue/index.js';
import type { LighterRhClient } from './lighter-rh.js';
import type { LighterMarket, Sizer } from './index.js';

export interface SizerOptions {
  readonly config: Config;
  readonly logger: Logger;
  readonly fairvalue: FairValueModule;
  readonly client: LighterRhClient;
  readonly now?: () => number;
}

/**
 * Truncate a value to a market's step size.
 *
 * A value already sitting on a step can land a hair below it in binary
 * floating point, so a value within a millionth of a step is treated as being
 * on it. Everything else rounds towards zero: a hedge is never larger than the
 * exposure it cancels because of rounding.
 */
export function truncateToStep(value: number, decimals: number): number {
  if (!Number.isFinite(value)) return 0;
  const factor = 10 ** decimals;
  const scaled = value * factor;
  const nearest = Math.round(scaled);
  const stepped = Math.abs(scaled - nearest) < 1e-6 ? nearest : Math.trunc(scaled);
  return stepped / factor;
}

/** Position sizing against the Lighter Robinhood Chain perpetuals. */
export class HedgeSizer implements Sizer {
  readonly #config: Config;
  readonly #logger: Logger;
  readonly #fairvalue: FairValueModule;
  readonly #client: LighterRhClient;
  readonly #now: () => number;

  constructor(options: SizerOptions) {
    this.#config = options.config;
    this.#logger = options.logger.child({ component: 'hedge.sizer' });
    this.#fairvalue = options.fairvalue;
    this.#client = options.client;
    this.#now = options.now ?? Date.now;
  }

  /** Stock exposure carried by a paired position, `qtyX x ratio x reference`. */
  async stockLegForPaired(input: {
    qtyX: number;
    ratio: number;
    stockSymbol: string;
  }): Promise<{ stockLegUsd: Quantity; referencePrice: Quantity }> {
    const referencePrice = await this.#reference(input.stockSymbol);
    const shares = input.qtyX * input.ratio;
    return {
      stockLegUsd: quantity(shares * referencePrice.value, 'USD', 'derived', this.#now()),
      referencePrice,
    };
  }

  /** Stock exposure carried by a direct holding, `qty x reference`. */
  async stockLegForToken(input: {
    symbol: string;
    qty: number;
  }): Promise<{ stockLegUsd: Quantity; referencePrice: Quantity }> {
    const referencePrice = await this.#reference(input.symbol);
    return {
      stockLegUsd: quantity(input.qty * referencePrice.value, 'USD', 'derived', this.#now()),
      referencePrice,
    };
  }

  /**
   * Round a base size to something the market accepts.
   *
   * Sizes truncate to the market's `size_decimals`. A size that survives
   * truncation but sits under `min_base_amount` becomes the floor, because the
   * market takes the floor or nothing. {@link HedgeSizer.plan} reports the
   * untruncated target separately, so an order that would overshoot is refused
   * rather than sent at the floor.
   */
  roundSize(sizeBase: number, market: LighterMarket): number {
    const magnitude = Math.abs(sizeBase);
    const stepped = truncateToStep(magnitude, market.sizeDecimals);
    if (stepped <= 0) return 0;
    const floor = truncateToStep(market.minBaseAmount, market.sizeDecimals);
    return stepped < floor ? floor : stepped;
  }

  /**
   * The full picture for one stock leg: the short it needs, the short it has,
   * the gap between them, and whether that gap can be sent.
   */
  async plan(input: { symbol: string; stockLegUsd: number }): Promise<HedgePlan> {
    const symbol = input.symbol.trim().toUpperCase();
    const market = await this.#client.perpFor(symbol);
    if (market === undefined) {
      throw new NotFoundError(`A ${symbol} perpetual on the Lighter Robinhood Chain instance`, { symbol });
    }

    const markPrice = await this.#client.markPrice(market.marketId);
    if (!(markPrice.value > 0)) {
      throw new NoReferenceError(symbol, ['lighter-rh mark price']);
    }

    const positions = await this.#client.positions();
    const current = positions.find((position) => position.symbol.toUpperCase() === symbol);
    const currentShort = current === undefined ? 0 : Math.max(0, -current.sizeBase.value);

    const rawTarget = input.stockLegUsd / markPrice.value;
    const targetShort = truncateToStep(rawTarget, market.sizeDecimals);
    const rawGap = targetShort - currentShort;
    const gap = Math.abs(rawGap);
    const driftBps = targetShort > 0 ? (rawGap / targetShort) * 10_000 : currentShort > 0 ? -10_000 : 0;

    const action = chooseAction({
      targetShort,
      currentShort,
      driftBps,
      driftLimitBps: this.#config.hedge.driftBps,
    });
    const orderSize = action === 'unwind' ? currentShort : gap;
    const fundingBps8h = await this.#funding(market.marketId);

    const asOf = this.#now();
    const base: HedgePlan = {
      symbol,
      marketId: market.marketId,
      stockLegNotionalUsd: quantity(input.stockLegUsd, 'USD', 'derived', asOf),
      referencePrice: markPrice,
      targetShortBase: quantity(targetShort, 'token', 'derived', asOf),
      ...(current === undefined ? {} : { currentShortBase: current.sizeBase }),
      driftBps: quantity(driftBps, 'bps', 'derived', asOf),
      action,
      ...(action === 'none' ? {} : { actionSizeBase: quantity(orderSize, 'token', 'derived', asOf) }),
      ...(fundingBps8h === undefined ? {} : { fundingBps8h }),
      executable: false,
      asOf,
    };

    const reason = this.#blockingReason(base, market, orderSize);
    return reason === undefined ? { ...base, executable: true } : { ...base, executable: false, reason };
  }

  /** Why this plan cannot be sent, or undefined when it can. */
  #blockingReason(plan: HedgePlan, market: LighterMarket, orderSize: number): string | undefined {
    if (plan.action === 'none') {
      return `The ${plan.symbol} short is within ${Math.round(Math.abs(plan.driftBps?.value ?? 0))} bps of target, so no order is needed.`;
    }
    const floor = market.minBaseAmount;
    if (orderSize > 0 && orderSize < floor) {
      return `Closing the ${plan.symbol} gap needs a ${orderSize.toFixed(market.sizeDecimals)} order and the smallest ${plan.symbol} order the venue accepts is ${floor.toFixed(market.sizeDecimals)}.`;
    }
    const readiness = this.#client.readiness();
    if (!readiness.ok) return readiness.reason;
    return undefined;
  }

  async #reference(symbol: string): Promise<Quantity> {
    const ticker = symbol.trim().toUpperCase();
    const selection = await this.#fairvalue.reference.select(ticker, this.#now());
    if (selection.chosen === undefined) {
      throw new NoReferenceError(ticker, selection.candidates.map((candidate) => candidate.source));
    }
    return selection.chosen.price;
  }

  async #funding(marketId: number): Promise<Quantity | undefined> {
    try {
      return await this.#client.funding(marketId);
    } catch (error) {
      this.#logger.debug('funding unavailable', { marketId, reason: (error as Error).message });
      return undefined;
    }
  }
}

/** Pick the move that closes the gap between the short held and the short needed. */
export function chooseAction(input: {
  targetShort: number;
  currentShort: number;
  driftBps: number;
  driftLimitBps: number;
}): HedgePlan['action'] {
  if (input.targetShort <= 0) return input.currentShort > 0 ? 'unwind' : 'none';
  if (input.currentShort <= 0) return 'open';
  if (Math.abs(input.driftBps) <= input.driftLimitBps) return 'none';
  return input.driftBps > 0 ? 'increaseShort' : 'decreaseShort';
}
