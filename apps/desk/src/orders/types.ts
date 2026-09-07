/**
 * Contracts for the orders module.
 *
 * Every interface the rest of the desk touches is declared here and
 * re-exported from `index.ts`, so a surface can depend on the shape of an
 * order without depending on how the engine evaluates one.
 */

import type { Address, Execution, Order, OrderBounds, OrderKind, OrderSide, OrderTrigger, Source } from '../core/types.js';
import type { Config } from '../core/config.js';
import type { Keystore } from '../core/keystore.js';
import type { Logger } from '../core/logger.js';
import type { Store } from '../core/store.js';
import type { ChainModule } from '../chain/index.js';
import type { FairValueModule } from '../fairvalue/index.js';

export interface OrdersDeps {
  readonly config: Config;
  readonly logger: Logger;
  readonly store: Store;
  readonly chain: ChainModule;
  readonly fairvalue: FairValueModule;
  /**
   * Keys for intent signing. Loaded from the desk home on first use when the
   * caller does not supply one.
   */
  readonly keystore?: Keystore;
  /** Prices the engine evaluates triggers against. Defaults to the fair value module. */
  readonly snapshots?: SnapshotProvider;
  /** Clock. Tests supply their own. */
  readonly now?: () => number;
}

/** What a caller supplies to create an order. */
export interface CreateOrderInput {
  readonly kind: OrderKind;
  readonly side: OrderSide;
  readonly tokenIn: Address;
  readonly tokenOut: Address;
  /** Amount to sell, smallest units of `tokenIn`. */
  readonly amountIn: bigint;
  readonly trigger: OrderTrigger;
  /** Overrides for the configured bounds. Values above the config limits are refused. */
  readonly bounds?: Partial<OrderBounds>;
  /** Ask for a signed transaction. Requires `config.live` as well. Default false. */
  readonly live?: boolean;
  /** ms since epoch. Null is good until cancelled. */
  readonly expiresAt?: number | null;
  /** For `kind: 'oco'`, the two branches. First fill cancels the sibling. */
  readonly legs?: readonly [Omit<CreateOrderInput, 'legs' | 'kind'>, Omit<CreateOrderInput, 'legs' | 'kind'>];
}

/** Prices the engine evaluates a trigger against. */
export interface TriggerContext {
  /** USD per whole token from the pool. */
  readonly usdOnchain?: number;
  /** USD per whole token from the selected reference. */
  readonly usdFair?: number;
  /** Stock leg premium, basis points. */
  readonly premiumBps?: number;
  /** Next United States regular open, ms since epoch. */
  readonly nextOpen?: number;
  /** Evaluation time, ms since epoch. */
  readonly now: number;
}

/**
 * One reading of what an order's subject token is worth.
 *
 * The subject of a buy is `tokenOut`, the subject of a sell is `tokenIn`: the
 * token whose price the trader is waiting on.
 */
export interface OrderSnapshot {
  readonly token: Address;
  readonly symbol?: string;
  /** USD per whole token from the pool. */
  readonly usdOnchain?: number;
  /** USD per whole token from the selected reference. */
  readonly usdFair?: number;
  /** Stock leg premium, basis points. */
  readonly premiumBps?: number;
  /** Source that produced the reference price. */
  readonly source?: Source;
  /** Observation time, ms since epoch. */
  readonly asOf: number;
  /** Plain reason when a price is missing. */
  readonly note?: string;
}

/** Where the engine reads prices from. The fair value module is the default. */
export interface SnapshotProvider {
  snapshot(order: Order, now: number): Promise<OrderSnapshot>;
}

/** Result of one bounds check, with the number that tripped it. */
export interface BoundsCheck {
  readonly ok: boolean;
  readonly reason?: string;
  readonly bound?: string;
  readonly limit?: number;
  readonly actual?: number;
  readonly unit?: string;
}

/** Order lifecycle. */
export interface OrderBook {
  create(input: CreateOrderInput): Promise<Order[]>;
  get(id: string): Order | undefined;
  list(filter?: { status?: Order['status']; limit?: number }): Order[];
  /** Cancel an open order. Cancels the sibling of an OCO pair as well. */
  cancel(id: string, reason?: string): Promise<Order>;
}

/** Evaluation loop. */
export interface Engine {
  start(): void;
  stop(): void;
  running(): boolean;
  /** Evaluate every open order once. Returns the orders that fired. */
  tick(now?: number): Promise<Order[]>;
  /**
   * Pure trigger test. `true` when every condition present in the trigger holds.
   * A trigger with no conditions never fires.
   */
  isTriggered(trigger: OrderTrigger, context: TriggerContext): boolean;
  /** Apply notional, slippage, premium, and daily caps. Never throws. */
  checkBounds(order: Order, quotedUsd: number, premiumBps: number | undefined): BoundsCheck;
  /** Quote, check bounds, then send or simulate. Records the execution row. */
  execute(order: Order): Promise<Execution>;
  /** Filled and simulated notional over the last 24 hours, USD. */
  dailyNotionalUsd(now?: number): number;
}

/** EIP-712 payload an external filler could redeem within the same bounds. */
export interface DeskIntent {
  readonly tokenIn: Address;
  readonly tokenOut: Address;
  readonly maxAmountIn: bigint;
  readonly minAmountOut: bigint;
  /** Seconds since epoch. */
  readonly deadline: bigint;
  /** keccak256 of the canonical encoding of the order conditions. */
  readonly conditionsHash: `0x${string}`;
}

/** EIP-712 domain, types, primary type, and message for one intent. */
export interface DeskIntentTypedData extends Record<string, unknown> {
  readonly domain: {
    readonly name: string;
    readonly version: string;
    readonly chainId: number;
  };
  readonly types: Record<string, readonly { readonly name: string; readonly type: string }[]>;
  readonly primaryType: 'DeskIntent';
  readonly message: DeskIntent;
}

/**
 * Intent signing. The format ships in v1 so an external filler can be built
 * against it later. The desk does not run a filler.
 */
export interface Intents {
  /** EIP-712 domain and types for `DeskIntent`. */
  typedData(intent: DeskIntent): DeskIntentTypedData;
  /** Hash of the order conditions, bound into the signature. */
  conditionsHash(order: Order): `0x${string}`;
  /** Sign with the desk key. Never returns the key. */
  sign(intent: DeskIntent): Promise<`0x${string}`>;
  /** EIP-712 digest of an intent, the value the signature covers. */
  hash(intent: DeskIntent): `0x${string}`;
  /** Recover the signer and compare it against `expected`. */
  verify(intent: DeskIntent, signature: `0x${string}`, expected: Address): Promise<boolean>;
  /** Address the desk signs with. Throws when no key is available. */
  signerAddress(): Address;
  /** Build an intent from an order and a quote. */
  fromOrder(order: Order, options: { minAmountOut: bigint; deadlineSec?: number; now?: number }): DeskIntent;
}

export interface OrdersModule {
  readonly book: OrderBook;
  readonly engine: Engine;
  readonly intents: Intents;
}
