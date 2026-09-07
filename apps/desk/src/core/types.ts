/**
 * Shared value types for Covenant Desk.
 *
 * Rule for every module: a number that leaves the process carries its unit,
 * the source it came from, and the time it was observed. `Quantity` is that
 * envelope. Raw token amounts stay `bigint`; prices and ratios are `number`.
 */

/** Unit of a {@link Quantity}. */
export type Unit =
  /** United States dollars. */
  | 'USD'
  /** USDG stablecoin units (1 USDG is not assumed to be 1 USD). */
  | 'USDG'
  /** Smallest indivisible token unit, before decimals are applied. */
  | 'raw'
  /** Basis points. 100 bps = 1%. */
  | 'bps'
  /** Ether, 18 decimals. */
  | 'ETH'
  /** Whole tokens, decimals already applied. */
  | 'token'
  /** Underlying shares, ERC-8056 multiplier already applied. */
  | 'share'
  /** Seconds. */
  | 'sec';

/** Where a number came from. Never inferred, always recorded by the reader. */
export type Source =
  /** Chainlink feed on chain 4663, inside its publishing session. */
  | 'chainlink'
  /** Last Chainlink price, published before the current session. */
  | 'chainlink-stale'
  /** Lighter Robinhood Chain instance (perp mark, funding, position). */
  | 'lighter-rh'
  /** Robinhood assets API (`/rhj/assets`, `/rhj/prices`). */
  | 'rhj'
  /** Uniswap v4 pool state or quoter on chain 4663. */
  | 'pool'
  /** Computed by the desk from other quantities. */
  | 'derived'
  /** Read from local configuration. */
  | 'config'
  /** Read from the local store. */
  | 'store';

/** A number with its unit, its origin, and the time it was observed. */
export interface Quantity {
  /** Human-scale value. For `unit: 'raw'` read {@link Quantity.raw} instead. */
  readonly value: number;
  readonly unit: Unit;
  readonly source: Source;
  /** Observation time, milliseconds since the Unix epoch. */
  readonly asOf: number;
  /** Exact smallest-unit amount. Set whenever the value came from a token amount. */
  readonly raw?: bigint;
  /** Token decimals used to derive `value` from `raw`. */
  readonly decimals?: number;
  /** Block the value was read at, when it came from chain. */
  readonly blockNumber?: bigint;
}

/** Build a {@link Quantity} from a human-scale number. */
export function quantity(
  value: number,
  unit: Unit,
  source: Source,
  asOf: number = Date.now(),
  extra: Pick<Quantity, 'blockNumber'> = {},
): Quantity {
  return { value, unit, source, asOf, ...extra };
}

/** Build a {@link Quantity} from a smallest-unit token amount. */
export function rawQuantity(
  raw: bigint,
  decimals: number,
  source: Source,
  asOf: number = Date.now(),
  extra: Pick<Quantity, 'blockNumber'> = {},
): Quantity {
  return { value: toDecimalNumber(raw, decimals), unit: 'raw', source, asOf, raw, decimals, ...extra };
}

/**
 * Convert a smallest-unit amount to a JavaScript number.
 * Precision is lost above 2^53; use it for display and comparison, never for
 * calldata.
 */
export function toDecimalNumber(raw: bigint, decimals: number): number {
  if (decimals < 0) throw new RangeError(`decimals must be >= 0, got ${decimals}`);
  const negative = raw < 0n;
  const abs = negative ? -raw : raw;
  const base = 10n ** BigInt(decimals);
  const whole = abs / base;
  const frac = abs - whole * base;
  const value = Number(whole) + Number(frac) / Number(base);
  return negative ? -value : value;
}

/** Convert a human-scale amount to a smallest-unit amount, truncating extra digits. */
export function toRawAmount(value: number | string, decimals: number): bigint {
  if (decimals < 0) throw new RangeError(`decimals must be >= 0, got ${decimals}`);
  const text = typeof value === 'number' ? value.toFixed(Math.min(decimals, 20)) : value.trim();
  if (!/^-?\d*(\.\d*)?$/.test(text) || text === '' || text === '-') {
    throw new RangeError(`not a decimal amount: ${String(value)}`);
  }
  const negative = text.startsWith('-');
  const unsigned = negative ? text.slice(1) : text;
  const [whole = '0', frac = ''] = unsigned.split('.');
  const padded = (frac + '0'.repeat(decimals)).slice(0, decimals);
  const raw = BigInt(whole === '' ? '0' : whole) * 10n ** BigInt(decimals) + BigInt(padded === '' ? '0' : padded);
  return negative ? -raw : raw;
}

/** 20-byte address, lowercase or checksummed, `0x`-prefixed. */
export type Address = `0x${string}`;
/** 32-byte hash or pool id, `0x`-prefixed. */
export type Hex32 = `0x${string}`;

/** An ERC-20 on chain 4663. Stock tokens carry the ERC-8056 fields. */
export interface Token {
  readonly address: Address;
  readonly symbol: string;
  readonly name: string;
  readonly decimals: number;
  /** True when the token is a Robinhood stock token. */
  readonly isStockToken: boolean;
  /** ERC-8056 `uiMultiplier()`, shares per token. Stock tokens only. */
  readonly uiMultiplier?: number;
  /** Multiplier scheduled by `newUIMultiplier()`, if one is pending. */
  readonly pendingMultiplier?: number;
  /** `tokenPaused()`. Transfers are stopped while true. */
  readonly tokenPaused?: boolean;
  /** `oraclePaused()`. The issuer price feed is stopped while true. */
  readonly oraclePaused?: boolean;
  /** Chainlink equity feed for this symbol, when one is published on 4663. */
  readonly feed?: Address;
  /** Lighter Robinhood Chain perpetual market id, when the symbol is listed. */
  readonly lighterMarketId?: number;
  /** Venues the issuer accepts orders on, from the Robinhood assets API. */
  readonly tradingCapabilities?: {
    readonly market: boolean;
    readonly extended: boolean;
    readonly overnight: boolean;
  };
  readonly isin?: string;
  readonly logoUrl?: string;
}

/** A Uniswap v4 pool on chain 4663. */
export interface Pool {
  readonly poolId: Hex32;
  /** Lower-sorted currency. The zero address means native ETH. */
  readonly currency0: Address;
  readonly currency1: Address;
  readonly decimals0: number;
  readonly decimals1: number;
  /** Static fee in hundredths of a bip, or 0x800000 for a dynamic fee. */
  readonly fee: number;
  readonly tickSpacing: number;
  readonly hooks: Address;
  /** Block the `Initialize` log was emitted in. */
  readonly initialBlock: bigint;
  /** Fee currently charged, hundredths of a bip, from `StateView.getSlot0`. */
  readonly lpFee?: number;
  /** In-range liquidity from `StateView.getLiquidity`. */
  readonly liquidity?: bigint;
  readonly sqrtPriceX96?: bigint;
  readonly tick?: number;
  /** Price of currency0 in currency1, decimals applied on both sides. */
  readonly midPrice?: Quantity;
  /**
   * True when `lpFee` is above 300 bps. Routing must refuse these pools.
   * Chain 4663 carries USDG pools charging 65% to 90%.
   */
  readonly trap?: boolean;
}

/** One candidate answer for "what is this stock worth in USD right now". */
export interface ReferenceCandidate {
  readonly symbol: string;
  /** USD per whole token, ERC-8056 multiplier already applied. */
  readonly price: Quantity;
  readonly source: Source;
  /** Publication time of the underlying observation, ms since epoch. */
  readonly updatedAt: number;
  /** Age of the observation in seconds at selection time. */
  readonly ageSec: number;
  /** True when the candidate was published before the current session. */
  readonly stale: boolean;
  /** Plain reason this candidate was or was not chosen. */
  readonly note?: string;
}

/** State of the United States equities calendar. */
export type SessionState = 'open' | 'extended' | 'overnight' | 'closed';

/** On-chain price, reference price, and the premium between them. */
export interface FairValue {
  readonly symbol: string;
  readonly token: Address;
  /** Price the chain is charging, USD per whole token. */
  readonly onchainMid?: Quantity;
  /** Price the token should be at, USD per whole token. */
  readonly reference?: Quantity;
  /** Source that won selection. */
  readonly referenceSource?: Source;
  /** Every reference the desk could read, in selection order. */
  readonly candidates: readonly ReferenceCandidate[];
  /** `onchainMid / reference - 1`, in basis points. */
  readonly premiumBps?: Quantity;
  readonly sessionState: SessionState;
  /** Deepest non-trap pool the on-chain mid was read from. */
  readonly pool?: Hex32;
  readonly blockNumber?: bigint;
  readonly asOf: number;
}

/** A token quoted in a stock token, priced through its stock leg. */
export interface PairedQuote {
  readonly token: Address;
  readonly symbol: string;
  /** Stock token this pool quotes in. */
  readonly stockSymbol: string;
  readonly stockToken: Address;
  readonly pool: Hex32;
  /** Stock tokens per paired token, from the deepest non-trap pool. */
  readonly ratio: Quantity;
  /** `ratio x onchainMid(stock)`, USD per whole token. */
  readonly usdOnchain: Quantity;
  /** `ratio x reference(stock)`, USD per whole token. */
  readonly usdFair: Quantity;
  /** Premium carried by the stock leg, basis points. */
  readonly stockLegPremiumBps: Quantity;
  /** Price through an ETH pool, when one exists. */
  readonly usdViaWeth?: Quantity;
  /** Cheapest way in, by USD paid per token. */
  readonly bestEntryRoute?: RouteSummary;
  /** Best way out, by USD received per token. */
  readonly bestExitRoute?: RouteSummary;
  /** Every stock pool quoting this token, for multipool launches. */
  readonly alternatives?: readonly PairedQuote[];
  /** Liquidity-weighted fair value across all stock pools for this token. */
  readonly weightedUsdFair?: Quantity;
  readonly asOf: number;
}

/** One priced path through the pools. */
export interface RouteSummary {
  readonly pools: readonly Hex32[];
  readonly path: readonly Address[];
  /** USD per whole token along this path. */
  readonly usdPrice: Quantity;
  readonly liquidity?: bigint;
  readonly note?: string;
}

export type OrderKind = 'limit' | 'stop' | 'takeProfit' | 'oco' | 'atOpen' | 'premium';
export type OrderSide = 'buy' | 'sell';
export type OrderStatus = 'open' | 'triggered' | 'filled' | 'failed' | 'cancelled' | 'expired';

/** Condition that arms an order. Every field is optional; all present fields must hold. */
export interface OrderTrigger {
  /** Fire when the chosen price is at or below this USD level. */
  readonly priceLte?: number;
  /** Fire when the chosen price is at or above this USD level. */
  readonly priceGte?: number;
  /** Which price the price conditions read. Default `usdOnchain`. */
  readonly priceBasis?: 'usdOnchain' | 'usdFair';
  /** Fire when the stock leg premium is at or below this level, basis points. */
  readonly premiumLteBps?: number;
  /** Fire when the stock leg premium is at or above this level, basis points. */
  readonly premiumGteBps?: number;
  /** Fire this many seconds after the next United States regular open. */
  readonly atNextOpenOffsetSec?: number;
  /** Fire at this wall-clock time, ms since epoch. */
  readonly at?: number;
}

/** Limits applied to a single order at execution time. */
export interface OrderBounds {
  readonly maxSlippageBps: number;
  readonly maxOrderNotionalUsd: number;
  /** Highest stock-leg premium a buy may pay, basis points. */
  readonly maxBuyPremiumBps: number;
}

export interface Order {
  readonly id: string;
  readonly kind: OrderKind;
  readonly side: OrderSide;
  readonly tokenIn: Address;
  readonly tokenOut: Address;
  /** Amount to sell, smallest units of `tokenIn`. */
  readonly amountIn: bigint;
  readonly trigger: OrderTrigger;
  readonly bounds: OrderBounds;
  /** True asks for a signed transaction. Requires `config.live` as well. */
  readonly live: boolean;
  readonly status: OrderStatus;
  /** ms since epoch. */
  readonly createdAt: number;
  /** ms since epoch. Null means good until cancelled. */
  readonly expiresAt: number | null;
  /** Parent OCO order id, for the two children of an OCO pair. */
  readonly parentId: string | null;
  /** Plain reason for the current status. */
  readonly reason?: string;
}

/** A fill, or the fill that a dry run would have produced. */
export interface Execution {
  readonly id: string;
  readonly orderId: string;
  /** True when a transaction was signed and sent. */
  readonly live: boolean;
  readonly txHash?: Hex32;
  readonly amountIn: bigint;
  readonly amountOut: bigint;
  /** `tokenOut` per `tokenIn`, decimals applied. */
  readonly effectivePrice: Quantity;
  /** Quote used to size `minAmountOut`, before slippage. */
  readonly quotedAmountOut: bigint;
  /** Size of the fill in USD. The rolling daily cap is measured on this. */
  readonly notionalUsd: Quantity;
  readonly slippageBps: number;
  readonly gasUsed?: bigint;
  readonly blockNumber?: bigint;
  readonly status: 'simulated' | 'sent' | 'confirmed' | 'reverted';
  readonly reason?: string;
  readonly createdAt: number;
}

/** A short sized to cancel the stock leg of a position. */
export interface HedgePlan {
  readonly symbol: string;
  readonly marketId?: number;
  /** Stock exposure being cancelled, USD. */
  readonly stockLegNotionalUsd: Quantity;
  /** Reference price the size was computed from. */
  readonly referencePrice: Quantity;
  /** Short size in base units, rounded to the market's `size_decimals`. */
  readonly targetShortBase: Quantity;
  /** Size held now, base units. Negative means short. */
  readonly currentShortBase?: Quantity;
  /** Difference between target and current, basis points of target. */
  readonly driftBps?: Quantity;
  /** Order the rebalancer would place to close the gap. */
  readonly action: 'none' | 'increaseShort' | 'decreaseShort' | 'open' | 'unwind';
  readonly actionSizeBase?: Quantity;
  /** Funding rate on this market over eight hours, basis points. */
  readonly fundingBps8h?: Quantity;
  /** True when the plan can be sent. False keeps it a plan. */
  readonly executable: boolean;
  /** Plain reason when `executable` is false. */
  readonly reason?: string;
  readonly asOf: number;
}

/** A live position on the Lighter Robinhood Chain instance. */
export interface HedgePosition {
  readonly symbol: string;
  readonly marketId: number;
  /** Signed base size. Negative is short. */
  readonly sizeBase: Quantity;
  readonly entryPrice?: Quantity;
  readonly markPrice: Quantity;
  readonly unrealizedPnlUsd?: Quantity;
  readonly fundingBps8h?: Quantity;
  readonly asOf: number;
}

/** One recorder row: what the chain charged and what the reference said. */
export interface Observation {
  readonly id?: number;
  /** ms since epoch. */
  readonly ts: number;
  readonly symbol: string;
  readonly token: Address;
  readonly pool?: Hex32;
  /** USD per whole token from the pool. */
  readonly onchainMidUsd?: number;
  /** USD per whole token from the selected reference. */
  readonly referenceUsd?: number;
  readonly referenceSource?: Source;
  readonly premiumBps?: number;
  readonly sessionState: SessionState;
  readonly blockNumber?: bigint;
  /** Every reference the desk could read at this instant. */
  readonly candidates?: readonly ReferenceCandidate[];
  /** Set for paired pools: the stock token this pool quotes in. */
  readonly stockSymbol?: string;
  readonly liquidity?: bigint;
}

/** What the desk is doing right now. */
export interface DeskStatus {
  readonly version: string;
  /** Seconds since the daemon started. */
  readonly uptimeSec: number;
  /** True when signed execution is permitted by config. */
  readonly live: boolean;
  readonly acknowledgedRestrictions: boolean;
  readonly chainId: number;
  readonly blockNumber?: bigint;
  readonly sessionState: SessionState;
  /** Next United States regular open, ms since epoch. */
  readonly nextOpen?: number;
  readonly tokensTracked: number;
  readonly poolsTracked: number;
  /** True while the desk is looking for pools it has not seen yet. */
  readonly poolScanRunning: boolean;
  /** Highest block the pool scan has covered. Absent before the first pass finishes. */
  readonly poolScanBlock?: bigint;
  readonly openOrders: number;
  readonly observations24h: number;
  readonly recorderRunning: boolean;
  readonly hedgeEnabled: boolean;
  /** Loops that reported a failure on their last pass, with the reason. */
  readonly degraded: readonly { readonly component: string; readonly reason: string }[];
  readonly asOf: number;
}

/** An audit row. Every order status change writes one. */
export interface DeskEvent {
  readonly id?: number;
  readonly ts: number;
  readonly kind: string;
  readonly subject?: string;
  readonly detail?: Record<string, unknown>;
}
