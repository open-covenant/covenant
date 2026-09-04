/**
 * New York Stock Exchange calendar.
 *
 * Full closures and early closes for 2025, 2026, and 2027, written as New York
 * calendar dates. The desk needs 2026; the neighbouring years are here so a
 * clock that crosses a year end still answers. Dates outside the covered years
 * are treated as ordinary weekdays and the session reports that in its note,
 * because guessing a holiday is worse than saying the calendar ends.
 */

import type { CivilDate } from './time.js';
import { isoDate } from './time.js';

/** A day the exchange is closed, or closes early. */
export interface MarketHoliday {
  /** `YYYY-MM-DD` in New York. */
  readonly date: string;
  readonly name: string;
}

/** Full closures. Trading does not run at all on these dates. */
export const MARKET_HOLIDAYS: readonly MarketHoliday[] = [
  { date: '2025-01-01', name: "New Year's Day" },
  { date: '2025-01-09', name: 'National day of mourning' },
  { date: '2025-01-20', name: 'Martin Luther King Jr. Day' },
  { date: '2025-02-17', name: "Washington's Birthday" },
  { date: '2025-04-18', name: 'Good Friday' },
  { date: '2025-05-26', name: 'Memorial Day' },
  { date: '2025-06-19', name: 'Juneteenth' },
  { date: '2025-07-04', name: 'Independence Day' },
  { date: '2025-09-01', name: 'Labor Day' },
  { date: '2025-11-27', name: 'Thanksgiving Day' },
  { date: '2025-12-25', name: 'Christmas Day' },

  { date: '2026-01-01', name: "New Year's Day" },
  { date: '2026-01-19', name: 'Martin Luther King Jr. Day' },
  { date: '2026-02-16', name: "Washington's Birthday" },
  { date: '2026-04-03', name: 'Good Friday' },
  { date: '2026-05-25', name: 'Memorial Day' },
  { date: '2026-06-19', name: 'Juneteenth' },
  { date: '2026-07-03', name: 'Independence Day, observed' },
  { date: '2026-09-07', name: 'Labor Day' },
  { date: '2026-11-26', name: 'Thanksgiving Day' },
  { date: '2026-12-25', name: 'Christmas Day' },

  { date: '2027-01-01', name: "New Year's Day" },
  { date: '2027-01-18', name: 'Martin Luther King Jr. Day' },
  { date: '2027-02-15', name: "Washington's Birthday" },
  { date: '2027-03-26', name: 'Good Friday' },
  { date: '2027-05-31', name: 'Memorial Day' },
  { date: '2027-06-18', name: 'Juneteenth, observed' },
  { date: '2027-07-05', name: 'Independence Day, observed' },
  { date: '2027-09-06', name: 'Labor Day' },
  { date: '2027-11-25', name: 'Thanksgiving Day' },
  { date: '2027-12-24', name: 'Christmas Day, observed' },
];

/** Days the regular session ends at 13:00 New York time. */
export const EARLY_CLOSES: readonly MarketHoliday[] = [
  { date: '2025-07-03', name: 'Day before Independence Day' },
  { date: '2025-11-28', name: 'Day after Thanksgiving' },
  { date: '2025-12-24', name: 'Christmas Eve' },

  { date: '2026-11-27', name: 'Day after Thanksgiving' },
  { date: '2026-12-24', name: 'Christmas Eve' },

  { date: '2027-11-26', name: 'Day after Thanksgiving' },
  { date: '2027-12-23', name: 'Day before Christmas Day, observed' },
];

/** Years the lists above cover. */
export const COVERED_YEARS: readonly number[] = [2025, 2026, 2027];

/** Minute of the New York day the regular session ends on an early close. */
export const EARLY_CLOSE_MINUTE = 13 * 60;

/** Holiday lookups for the session calendar. */
export interface HolidayCalendar {
  /** Name of the closure on this date, or undefined when the market trades. */
  holiday(date: CivilDate): string | undefined;
  /** Name of the early close on this date, or undefined for a full day. */
  earlyClose(date: CivilDate): string | undefined;
  /** True when the calendar carries data for this year. */
  covers(year: number): boolean;
}

export interface HolidayCalendarOptions {
  /** Extra closures, merged over the published list. */
  readonly holidays?: readonly MarketHoliday[];
  /** Extra early closes, merged over the published list. */
  readonly earlyCloses?: readonly MarketHoliday[];
  /** Extra years to treat as covered. */
  readonly years?: readonly number[];
}

/** Build the calendar. Tests pass their own dates through the options. */
export function createHolidayCalendar(options: HolidayCalendarOptions = {}): HolidayCalendar {
  const closures = new Map<string, string>();
  for (const entry of [...MARKET_HOLIDAYS, ...(options.holidays ?? [])]) closures.set(entry.date, entry.name);

  const early = new Map<string, string>();
  for (const entry of [...EARLY_CLOSES, ...(options.earlyCloses ?? [])]) early.set(entry.date, entry.name);

  const years = new Set<number>([...COVERED_YEARS, ...(options.years ?? [])]);
  for (const key of closures.keys()) {
    const year = Number(key.slice(0, 4));
    if (Number.isInteger(year)) years.add(year);
  }

  return {
    holiday: (date) => closures.get(isoDate(date)),
    earlyClose: (date) => early.get(isoDate(date)),
    covers: (year) => years.has(year),
  };
}
