/**
 * Fair value: what the chain is charging, what the token is worth, and the
 * premium between them.
 *
 * The desk exists for the hours when Wall Street is closed and the Chainlink
 * feed is frozen, so reference selection is explicit and every candidate is
 * returned alongside the one that won.
 */

import type { FairValueDeps, FairValueModule } from './contracts.js';
import type { HolidayCalendar } from './holidays.js';
import { createSession } from './session.js';
import { createReference } from './reference.js';
import { createPremium } from './premium.js';
import { createPaired } from './paired.js';
import { createRecorder } from './recorder.js';
import type { LighterMarkSource, RhjPriceSource } from './sources.js';
import { createLighterMarkSource, createRhjPriceSource } from './sources.js';

export * from './contracts.js';
export {
  addDays,
  dstEndUtc,
  dstStartUtc,
  etCivilToUtc,
  etDate,
  etOffsetMinutes,
  etParts,
  isDaylightSaving,
  isoDate,
  type CivilDate,
  type EtInstant,
} from './time.js';
export * from './holidays.js';
export * from './session.js';
export * from './sources.js';
export * from './reference.js';
export * from './premium.js';
export * from './paired.js';
export * from './recorder.js';
export * from './pools.js';

export interface FairValueOptions {
  /** Issuer quotes. Defaults to the Robinhood assets API. */
  readonly rhj?: RhjPriceSource;
  /** Perpetual marks. Defaults to the Lighter host in config. */
  readonly lighter?: LighterMarkSource;
  /** Exchange calendar. Defaults to the published holiday list. */
  readonly holidays?: HolidayCalendar;
  /** Clock, for tests. */
  readonly now?: () => number;
}

export function createFairValueModule(deps: FairValueDeps, options: FairValueOptions = {}): FairValueModule {
  const { config, logger, store, chain } = deps;
  const now = options.now;

  const session = createSession(options.holidays ? { holidays: options.holidays } : {});
  const rhj = options.rhj ?? createRhjPriceSource({ ...(now ? { now } : {}) });
  const lighter =
    options.lighter ?? createLighterMarkSource({ baseUrl: config.hedge.lighterBaseUrl, ...(now ? { now } : {}) });

  const reference = createReference({
    logger: logger.child({ component: 'reference' }),
    chain,
    session,
    rhj,
    lighter,
    ...(now ? { now } : {}),
  });

  const premium = createPremium({
    config,
    logger: logger.child({ component: 'premium' }),
    store,
    chain,
    session,
    reference,
    ...(now ? { now } : {}),
  });

  const paired = createPaired({
    config,
    logger: logger.child({ component: 'paired' }),
    store,
    chain,
    premium,
    ...(now ? { now } : {}),
  });

  const recorder = createRecorder({
    config,
    logger: logger.child({ component: 'recorder' }),
    store,
    chain,
    session,
    premium,
    paired,
    ...(now ? { now } : {}),
  });

  return { session, reference, premium, paired, recorder };
}
