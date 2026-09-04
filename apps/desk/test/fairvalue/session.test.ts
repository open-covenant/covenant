import { describe, expect, it } from 'vitest';
import { createSession } from '../../src/fairvalue/session.js';
import { createHolidayCalendar } from '../../src/fairvalue/holidays.js';
import { etCivilToUtc, etOffsetMinutes, etParts } from '../../src/fairvalue/time.js';

const session = createSession();

/** Instants are written in UTC so the test does not lean on the code it checks. */
const utc = (year: number, month: number, day: number, hour: number, minute = 0) =>
  Date.UTC(year, month - 1, day, hour, minute);

describe('New York clock', () => {
  it('follows the daylight saving rule without a time zone database', () => {
    expect(etOffsetMinutes(utc(2026, 1, 15, 17))).toBe(-300);
    expect(etOffsetMinutes(utc(2026, 7, 15, 17))).toBe(-240);
    // Second Sunday in March, 07:00 UTC.
    expect(etOffsetMinutes(utc(2026, 3, 8, 6, 59))).toBe(-300);
    expect(etOffsetMinutes(utc(2026, 3, 8, 7, 0))).toBe(-240);
    // First Sunday in November, 06:00 UTC.
    expect(etOffsetMinutes(utc(2026, 11, 1, 5, 59))).toBe(-240);
    expect(etOffsetMinutes(utc(2026, 11, 1, 6, 0))).toBe(-300);
  });

  it('reads wall-clock fields on both sides of a transition', () => {
    expect(etParts(utc(2026, 3, 9, 13, 30))).toMatchObject({ year: 2026, month: 3, day: 9, hour: 9, minute: 30 });
    expect(etParts(utc(2026, 11, 2, 14, 30))).toMatchObject({ year: 2026, month: 11, day: 2, hour: 9, minute: 30 });
  });

  it('round trips a wall-clock time through UTC', () => {
    expect(etCivilToUtc({ year: 2026, month: 7, day: 15 }, 9, 30)).toBe(utc(2026, 7, 15, 13, 30));
    expect(etCivilToUtc({ year: 2026, month: 1, day: 15 }, 9, 30)).toBe(utc(2026, 1, 15, 14, 30));
  });
});

describe('session state', () => {
  it('closes the week at 20:00 on Friday', () => {
    // Friday 2026-09-04, 19:59 and 20:01 New York time.
    expect(session.state(utc(2026, 9, 4, 23, 59))).toBe('extended');
    expect(session.state(utc(2026, 9, 5, 0, 1))).toBe('closed');
  });

  it('opens the week at 20:00 on Sunday', () => {
    // Sunday 2026-09-13, 19:59 and 20:01 New York time.
    expect(session.state(utc(2026, 9, 13, 23, 59))).toBe('closed');
    expect(session.state(utc(2026, 9, 14, 0, 1))).toBe('overnight');
  });

  it('names each part of a trading day', () => {
    const day = (hour: number, minute = 0) => session.state(etCivilToUtc({ year: 2026, month: 9, day: 8 }, hour, minute));
    expect(day(2)).toBe('overnight');
    expect(day(4)).toBe('extended');
    expect(day(9, 29)).toBe('extended');
    expect(day(9, 30)).toBe('open');
    expect(day(15, 59)).toBe('open');
    expect(day(16)).toBe('extended');
    expect(day(19, 59)).toBe('extended');
    expect(day(20, 1)).toBe('overnight');
  });

  it('closes all of a holiday and the evening before it', () => {
    // Labor Day, Monday 2026-09-07.
    expect(session.state(utc(2026, 9, 7, 14))).toBe('closed');
    expect(session.isHoliday(utc(2026, 9, 7, 14))).toBe(true);
    // Sunday evening leads into the holiday, so the week does not open.
    expect(session.state(utc(2026, 9, 7, 0, 1))).toBe('closed');
    // Independence Day observed, Friday 2026-07-03.
    expect(session.state(utc(2026, 7, 3, 14))).toBe('closed');
    // Thursday evening before it is closed as well.
    expect(session.state(utc(2026, 7, 3, 1))).toBe('closed');
  });

  it('ends the regular session at 13:00 on an early close', () => {
    // Friday 2026-11-27, standard time.
    expect(session.state(utc(2026, 11, 27, 17, 59))).toBe('open');
    expect(session.state(utc(2026, 11, 27, 18, 1))).toBe('extended');
    expect(session.info(utc(2026, 11, 27, 17, 59)).note).toContain('13:00');
  });

  it('keeps the market open across the day the clocks change', () => {
    // Sunday 2026-03-08 20:01 New York time is 00:01 UTC on the 9th under EDT.
    expect(session.state(utc(2026, 3, 9, 0, 1))).toBe('overnight');
    expect(session.state(utc(2026, 3, 9, 13, 30))).toBe('open');
    // Monday 2026-11-02 09:30 New York time is 14:30 UTC under EST.
    expect(session.state(utc(2026, 11, 2, 14, 30))).toBe('open');
    expect(session.state(utc(2026, 11, 2, 13, 30))).toBe('extended');
  });
});

describe('next open', () => {
  it('answers with today when the bell has not rung', () => {
    expect(session.nextOpen(utc(2026, 9, 8, 12))).toBe(utc(2026, 9, 8, 13, 30));
  });

  it('skips the weekend and the holiday behind it', () => {
    // Friday 2026-09-04 21:00 New York time. Monday is Labor Day.
    expect(session.nextOpen(utc(2026, 9, 5, 1))).toBe(utc(2026, 9, 8, 13, 30));
  });

  it('uses the offset in force on the day it lands on', () => {
    // Sunday before the clocks go forward, so Monday opens an hour earlier in UTC.
    expect(session.nextOpen(utc(2026, 3, 8, 16))).toBe(utc(2026, 3, 9, 13, 30));
    // Sunday after the clocks go back.
    expect(session.nextOpen(utc(2026, 11, 1, 16))).toBe(utc(2026, 11, 2, 14, 30));
  });
});

describe('session window', () => {
  it('runs Sunday 20:00 to Friday 20:00 in an ordinary week', () => {
    const window = session.window(utc(2026, 9, 16, 15));
    expect(window.running).toBe(true);
    expect(window.start).toBe(utc(2026, 9, 14, 0)); // Sunday 2026-09-13 20:00 New York time.
    expect(window.end).toBe(utc(2026, 9, 19, 0)); // Friday 2026-09-18 20:00 New York time.
  });

  it('covers the weekend as one closed span', () => {
    const window = session.window(utc(2026, 9, 19, 12));
    expect(window.running).toBe(false);
    expect(window.start).toBe(utc(2026, 9, 19, 0));
    expect(window.end).toBe(utc(2026, 9, 21, 0));
  });

  it('places a feed update inside or outside the current session', () => {
    const saturday = utc(2026, 9, 19, 12);
    const fridayClose = utc(2026, 9, 18, 20); // 16:00 New York time on Friday.
    expect(session.isWithinCurrentSession(saturday, fridayClose)).toBe(false);

    const tuesday = utc(2026, 9, 15, 15);
    expect(session.isWithinCurrentSession(tuesday, utc(2026, 9, 15, 14))).toBe(true);
    expect(session.isWithinCurrentSession(tuesday, utc(2026, 9, 11, 20))).toBe(false);
  });
});

describe('holiday calendar', () => {
  it('accepts dates a test supplies', () => {
    const custom = createSession({
      holidays: createHolidayCalendar({ holidays: [{ date: '2026-09-08', name: 'Test closure' }] }),
    });
    expect(custom.state(utc(2026, 9, 8, 15))).toBe('closed');
    expect(session.state(utc(2026, 9, 8, 15))).toBe('open');
  });

  it('says so when the year is outside the published list', () => {
    expect(session.info(utc(2031, 9, 8, 15)).note).toContain('2031');
  });
});
