import { describe, expect, it } from "vitest";
import { uptime } from "../_headerStats";

// uptime formats the header's "up" cell: elapsed time from the alpha-start
// timestamp to now, as "Nd Nh Nm", or an em-dash when the timestamp is
// unparseable or in the future. `now` is injected here so each case pins exact
// elapsed time — the fallback guards, the day/hour/minute decomposition, the
// hour-mod-24 / minute-mod-60 wraps, and the inclusive zero bound.

const SINCE = "2026-01-01T00:00:00.000Z";
const base = new Date(SINCE).getTime();
const at = (ms: number) => uptime(SINCE, base + ms);

describe("uptime", () => {
  it("returns an em-dash for an unparseable timestamp", () => {
    expect(uptime("not-a-date", base)).toBe("—");
  });

  it("returns an em-dash for a future timestamp", () => {
    expect(uptime(SINCE, base - 1000)).toBe("—");
  });

  it("renders zero elapsed as 0d 0h 0m (inclusive lower bound)", () => {
    expect(at(0)).toBe("0d 0h 0m");
  });

  it("decomposes days, hours, and minutes, dropping seconds", () => {
    const ms = 2 * 86_400_000 + 3_600_000 + 5 * 60_000 + 30_000;
    expect(at(ms)).toBe("2d 1h 5m");
  });

  it("wraps hours past 24 into the day count", () => {
    expect(at(25 * 3_600_000)).toBe("1d 1h 0m");
  });

  it("wraps minutes past 60 into the hour count", () => {
    expect(at(90 * 60_000)).toBe("0d 1h 30m");
  });
});
