/**
 * New York wall-clock arithmetic without a time zone library.
 *
 * The desk needs one time zone, America/New_York, and it needs the answer to
 * be the same on every machine and in every test. United States daylight
 * saving time has run to a fixed rule since 2007: clocks go forward on the
 * second Sunday in March at 02:00 local standard time (07:00 UTC) and back on
 * the first Sunday in November at 02:00 local daylight time (06:00 UTC). That
 * rule is small enough to compute directly, so this file does, and the
 * calendar above it stays deterministic.
 */

/** Milliseconds in one minute. */
const MINUTE_MS = 60_000;
/** Milliseconds in one day. */
export const DAY_MS = 86_400_000;
/** Eastern Standard Time, minutes from UTC. */
export const EST_OFFSET_MINUTES = -300;
/** Eastern Daylight Time, minutes from UTC. */
export const EDT_OFFSET_MINUTES = -240;

/** A calendar date in New York, month 1 to 12. */
export interface CivilDate {
  readonly year: number;
  readonly month: number;
  readonly day: number;
}

/** A wall-clock instant in New York, with the offset that produced it. */
export interface EtInstant extends CivilDate {
  readonly hour: number;
  readonly minute: number;
  readonly second: number;
  /** 0 is Sunday. */
  readonly weekday: number;
  /** Minutes from UTC: -300 in winter, -240 in summer. */
  readonly offsetMinutes: number;
}

/** UTC milliseconds of the `n`th `weekday` of a month. `weekday` 0 is Sunday. */
function nthWeekdayUtc(year: number, monthIndex: number, weekday: number, n: number): number {
  const first = Date.UTC(year, monthIndex, 1);
  const firstWeekday = new Date(first).getUTCDay();
  const shift = (weekday - firstWeekday + 7) % 7;
  return first + (shift + (n - 1) * 7) * DAY_MS;
}

/** Instant daylight saving time begins in New York, UTC milliseconds. */
export function dstStartUtc(year: number): number {
  return nthWeekdayUtc(year, 2, 0, 2) + 7 * 60 * MINUTE_MS;
}

/** Instant daylight saving time ends in New York, UTC milliseconds. */
export function dstEndUtc(year: number): number {
  return nthWeekdayUtc(year, 10, 0, 1) + 6 * 60 * MINUTE_MS;
}

/** Offset from UTC in New York at this instant, in minutes. */
export function etOffsetMinutes(ms: number): number {
  const year = new Date(ms).getUTCFullYear();
  return ms >= dstStartUtc(year) && ms < dstEndUtc(year) ? EDT_OFFSET_MINUTES : EST_OFFSET_MINUTES;
}

/** True while New York is on daylight saving time. */
export function isDaylightSaving(ms: number): boolean {
  return etOffsetMinutes(ms) === EDT_OFFSET_MINUTES;
}

/** Break an instant into New York wall-clock fields. */
export function etParts(ms: number): EtInstant {
  const offsetMinutes = etOffsetMinutes(ms);
  const shifted = new Date(ms + offsetMinutes * MINUTE_MS);
  return {
    year: shifted.getUTCFullYear(),
    month: shifted.getUTCMonth() + 1,
    day: shifted.getUTCDate(),
    hour: shifted.getUTCHours(),
    minute: shifted.getUTCMinutes(),
    second: shifted.getUTCSeconds(),
    weekday: shifted.getUTCDay(),
    offsetMinutes,
  };
}

/**
 * Turn a New York wall-clock time into UTC milliseconds.
 *
 * The hour that daylight saving time skips resolves forward, and the hour it
 * repeats resolves to the second pass. Every session boundary the desk uses
 * (04:00, 09:30, 13:00, 16:00, 20:00) sits outside both cases.
 */
export function etCivilToUtc(date: CivilDate, hour = 0, minute = 0, second = 0): number {
  const naive = Date.UTC(date.year, date.month - 1, date.day, hour, minute, second);
  const asStandard = naive - EST_OFFSET_MINUTES * MINUTE_MS;
  if (etOffsetMinutes(asStandard) === EDT_OFFSET_MINUTES) {
    const asDaylight = naive - EDT_OFFSET_MINUTES * MINUTE_MS;
    if (etOffsetMinutes(asDaylight) === EDT_OFFSET_MINUTES) return asDaylight;
  }
  return asStandard;
}

/** The New York calendar date at this instant. */
export function etDate(ms: number): CivilDate {
  const parts = etParts(ms);
  return { year: parts.year, month: parts.month, day: parts.day };
}

/** Days since 1970-01-01, used for date arithmetic that ignores the clock. */
export function dayIndex(date: CivilDate): number {
  return Date.UTC(date.year, date.month - 1, date.day) / DAY_MS;
}

/** Inverse of {@link dayIndex}. */
export function fromDayIndex(index: number): CivilDate {
  const at = new Date(index * DAY_MS);
  return { year: at.getUTCFullYear(), month: at.getUTCMonth() + 1, day: at.getUTCDate() };
}

/** Move a calendar date by whole days. */
export function addDays(date: CivilDate, days: number): CivilDate {
  return fromDayIndex(dayIndex(date) + days);
}

/** Day of the week for a calendar date. 0 is Sunday. */
export function weekdayOf(date: CivilDate): number {
  return new Date(Date.UTC(date.year, date.month - 1, date.day)).getUTCDay();
}

/** `YYYY-MM-DD`, the key the holiday calendar is written in. */
export function isoDate(date: CivilDate): string {
  const month = String(date.month).padStart(2, '0');
  const day = String(date.day).padStart(2, '0');
  return `${date.year}-${month}-${day}`;
}

/** True when the date falls Monday to Friday. */
export function isWeekday(date: CivilDate): boolean {
  const weekday = weekdayOf(date);
  return weekday >= 1 && weekday <= 5;
}
