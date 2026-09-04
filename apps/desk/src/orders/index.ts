/**
 * Conditional orders: the model, the trigger engine, and the bridge to
 * execution.
 *
 * Nothing signs a transaction unless the desk is live and the order was created
 * live. A dry run quotes, applies the same bounds, and records the fill it
 * would have produced.
 */

import { loadKeystore, type Keystore } from '../core/keystore.js';
import { createOrderBook } from './book.js';
import { createEngine } from './engine.js';
import { createIntents } from './intents.js';
import { createCachedSnapshotProvider, createFairValueSnapshotProvider } from './snapshot.js';
import type { OrdersContext } from './support.js';
import type { OrdersDeps, OrdersModule } from './types.js';

export * from './types.js';
export {
  AddressSchema,
  CreateOrderInputSchema,
  OrderBoundsOverrideSchema,
  OrderKindSchema,
  OrderSideSchema,
  OrderTriggerSchema,
  RawAmountSchema,
  countConditions,
  resolveBounds,
} from './schema.js';
export type { ParsedCreateOrderInput, ParsedLeg } from './schema.js';
export { OrderInvalidError, cancelSiblings, parseInput, settleParent } from './book.js';
export { isContainer, realizedSlippageBps } from './engine.js';
export {
  DEFAULT_INTENT_TTL_SEC,
  INTENT_DOMAIN_NAME,
  INTENT_DOMAIN_VERSION,
  INTENT_TYPES,
  canonicalConditions,
  createIntents,
} from './intents.js';
export { createCachedSnapshotProvider, createFairValueSnapshotProvider } from './snapshot.js';
export { DAY_MS, subjectToken } from './support.js';
export type { OrdersContext } from './support.js';

/** Build the order book, the engine, and the intent signer over one store. */
export function createOrdersModule(deps: OrdersDeps): OrdersModule {
  const ctx: OrdersContext = {
    config: deps.config,
    logger: deps.logger,
    store: deps.store,
    chain: deps.chain,
    fairvalue: deps.fairvalue,
    snapshots:
      deps.snapshots ??
      createCachedSnapshotProvider(
        createFairValueSnapshotProvider({
          store: deps.store,
          logger: deps.logger,
          fairvalue: deps.fairvalue,
          chain: deps.chain,
        }),
        Math.max(1_000, deps.config.engineIntervalSec * 800),
      ),
    now: deps.now ?? Date.now,
  };

  let keystore: Keystore | undefined = deps.keystore;
  const privateKey = (): string => {
    keystore ??= loadKeystore({});
    return keystore.require('DESK_EVM_PRIVATE_KEY');
  };

  return {
    book: createOrderBook(ctx),
    engine: createEngine(ctx),
    intents: createIntents({ chainId: deps.config.chainId, privateKey, now: ctx.now }),
  };
}
