/**
 * United States equities calendar.
 *
 * Chainlink publishes its equity feeds on a 24/5 schedule that runs from
 * Sunday 20:00 New York time to Friday 20:00 New York time and stops on
 * exchange holidays. Inside that window the desk names where the clock is:
 *
 *   overnight  20:00 to 04:00
 *   extended   04:00 to 09:30 and 16:00 to 20:00
 *   open       09:30 to 16:00, or 09:30 to 13:00 on an early close
 *   closed     weekends, holidays, and the evening before either
 *
 * A trading window belongs to the day it ends on, so 21:00 on Sunday is the
 * start of Monday's overnight session and 21:00 on Friday is closed. That one
 * rule also handles the night before a holiday without a special case.
 */

import type { SessionState } from '../core/types.js';
import type { Session, SessionInfo } from './contracts.js';
import type { HolidayCalendar } from './holidays.js';
import { EARLY_CLOSE_MINUTE, createHolidayCalendar } from './holidays.js';
import type { CivilDate } from './time.js';
import { addDays, etCivilToUtc, etDate, etParts, isWeekday, isoDate } from './time.js';

/** Minute of the New York day each boundary falls on. */
const OVERNIGHT_START_MINUTE = 20 * 60;
const OVERNIGHT_END_MINUTE = 4 * 60;
const REGULAR_OPEN_MINUTE = 9 * 60 + 30;
const REGULAR_CLOSE_MINUTE = 16 * 60;

/** How far the window search walks before it gives up. */
const MAX_WINDOW_DAYS = 21;
/** How far {@link Session.nextOpen} searches for the next trading day. */
const MAX_OPEN_SEARCH_DAYS = 40;

/** The contiguous span of days that share the running state around an instant. */
export interface SessionWindow {
  /** Start of the span, ms since epoch. */
  readonly start: number;
  /** End of the span, ms since epoch. */
  readonly end: number;
  /** True when the feeds publish across this span. */
  readonly running: boolean;
  /** First trading day the span covers, `YYYY-MM-DD` in New York. */
  readonly firstDay: string;
  /** Last trading day the span covers, `YYYY-MM-DD` in New York. */
  readonly lastDay: string;
}

/** The session calendar, plus the window and trading-day helpers built on it. */
export interface DeskSession extends Session {
  /** The contiguous open or closed span containing this instant. */
  window(now: number): SessionWindow;
  /** True when the exchange trades on the New York date of this instant. */
  isTradingDay(now: number): boolean;
  /** The trading day an instant belongs to, `YYYY-MM-DD` in New York. */
  sessionDay(now: number): string;
}

export interface SessionOptions {
  /** Holiday source. Defaults to the published exchange calendar. */
  readonly holidays?: HolidayCalendar;
}

/** Build the session calendar. */
export function createSession(options: SessionOptions = {}): DeskSession {
  const calendar = options.holidays ?? createHolidayCalendar();

  const tradingDate = (date: CivilDate): boolean => isWeekday(date) && calendar.holiday(date) === undefined;

  /** The calendar date whose session an instant belongs to. */
  const sessionDate = (now: number): CivilDate => {
    const parts = etParts(now);
    const today: CivilDate = { year: parts.year, month: parts.month, day: parts.day };
    return minuteOfDay(parts) >= OVERNIGHT_START_MINUTE ? addDays(today, 1) : today;
  };

  const state = (now: number): SessionState => {
    const parts = etParts(now);
    const day = sessionDate(now);
    if (!tradingDate(day)) return 'closed';

    const minute = minuteOfDay(parts);
    if (minute >= OVERNIGHT_START_MINUTE) return 'overnight';
    if (minute < OVERNIGHT_END_MINUTE) return 'overnight';
    if (minute < REGULAR_OPEN_MINUTE) return 'extended';
    const closeMinute = calendar.earlyClose(day) !== undefined ? EARLY_CLOSE_MINUTE : REGULAR_CLOSE_MINUTE;
    if (minute < closeMinute) return 'open';
    return 'extended';
  };

  const window = (now: number): SessionWindow => {
    const day = sessionDate(now);
    const running = tradingDate(day);

    let first = day;
    for (let step = 1; step <= MAX_WINDOW_DAYS; step += 1) {
      const previous = addDays(day, -step);
      if (tradingDate(previous) !== running) break;
      first = previous;
    }

    let last = day;
    for (let step = 1; step <= MAX_WINDOW_DAYS; step += 1) {
      const next = addDays(day, step);
      if (tradingDate(next) !== running) break;
      last = next;
    }

    return {
      start: etCivilToUtc(addDays(first, -1), 20, 0),
      end: etCivilToUtc(last, 20, 0),
      running,
      firstDay: isoDate(first),
      lastDay: isoDate(last),
    };
  };

  const nextOpen = (now: number): number => {
    const start = etDate(now);
    for (let step = 0; step <= MAX_OPEN_SEARCH_DAYS; step += 1) {
      const day = addDays(start, step);
      if (!tradingDate(day)) continue;
      const open = etCivilToUtc(day, 9, 30);
      if (open >= now) return open;
    }
    throw new RangeError(`No trading day found within ${MAX_OPEN_SEARCH_DAYS} days of ${new Date(now).toISOString()}`);
  };

  const info = (now: number): SessionInfo => {
    const current = state(now);
    const span = window(now);
    const day = sessionDate(now);
    const notes: string[] = [];

    const holiday = calendar.holiday(etDate(now));
    if (holiday) notes.push(`${holiday}, the exchange is closed`);
    const early = calendar.earlyClose(day);
    if (early && current !== 'closed') notes.push(`${early}, the regular session ends at 13:00 New York time`);
    if (!calendar.covers(etDate(now).year)) {
      notes.push(`The holiday calendar does not cover ${etDate(now).year}, so only weekends are treated as closed`);
    }

    return {
      state: current,
      nextOpen: nextOpen(now),
      ...(span.running ? { sessionEnd: span.end } : {}),
      ...(notes.length > 0 ? { note: notes.join('. ') } : {}),
    };
  };

  return {
    state,
    info,
    nextOpen,
    window,
    isHoliday: (now) => calendar.holiday(etDate(now)) !== undefined,
    isTradingDay: (now) => tradingDate(etDate(now)),
    sessionDay: (now) => isoDate(sessionDate(now)),
    isWithinCurrentSession: (now, updatedAt) => {
      const span = window(now);
      return updatedAt >= span.start && updatedAt < span.end;
    },
  };
}

function minuteOfDay(parts: { hour: number; minute: number }): number {
  return parts.hour * 60 + parts.minute;
}
