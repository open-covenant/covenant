import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import {
  formatCvnt,
  formatPercent,
  formatSol,
  formatWithGrouping,
  lockEndDate,
  parseCvntInput,
  relativeFromNow,
  shortAddr,
  tierLabel,
  tierMultiplier,
  trailingAnnualisedRate,
} from "../format";

describe("parseCvntInput", () => {
  it("scales whole and fractional input by 10^6", () => {
    expect(parseCvntInput("1")).toBe(1_000_000n);
    expect(parseCvntInput("1.5")).toBe(1_500_000n);
    expect(parseCvntInput("1.234567")).toBe(1_234_567n);
    expect(parseCvntInput("0.000001")).toBe(1n);
  });

  it("strips thousands separators and surrounding whitespace", () => {
    expect(parseCvntInput("1,234.5")).toBe(1_234_500_000n);
    expect(parseCvntInput("  10  ")).toBe(10_000_000n);
  });

  it("treats a trailing dot as a whole number", () => {
    expect(parseCvntInput("1.")).toBe(1_000_000n);
  });

  it("rejects empty, blank, non-numeric, and over-precise input", () => {
    expect(parseCvntInput("")).toBeNull();
    expect(parseCvntInput("   ")).toBeNull();
    expect(parseCvntInput("abc")).toBeNull();
    expect(parseCvntInput("1.1234567")).toBeNull();
    expect(parseCvntInput(".5")).toBeNull();
    expect(parseCvntInput("-5")).toBeNull();
  });
});

describe("formatCvnt", () => {
  it("scales by 6 decimals and trims trailing fractional zeros", () => {
    expect(formatCvnt(1_234_567_890n)).toBe("1,234.56");
    expect(formatCvnt(1_500_000n)).toBe("1.5");
    expect(formatCvnt(2_000_000n)).toBe("2");
  });

  it("groups thousands in the whole part", () => {
    expect(formatCvnt(1_000_000_000_000n)).toBe("1,000,000");
  });

  it("honors maxFrac (0 drops the fraction, higher keeps more)", () => {
    expect(formatCvnt(1_234_567_890n, { maxFrac: 0 })).toBe("1,234");
    expect(formatCvnt(1_234_560n, { maxFrac: 6 })).toBe("1.23456");
  });
});

describe("formatSol", () => {
  it("scales by 9 decimals with a 4-place default and trims zeros", () => {
    expect(formatSol(1_500_000_000n)).toBe("1.5");
    expect(formatSol(1_000_000_000n)).toBe("1");
    expect(formatSol(1_234_500_000n)).toBe("1.2345");
  });

  it("honors maxFrac=0", () => {
    expect(formatSol(2_500_000_000n, { maxFrac: 0 })).toBe("2");
  });
});

describe("formatWithGrouping", () => {
  it("leaves up to three digits ungrouped", () => {
    expect(formatWithGrouping(0n)).toBe("0");
    expect(formatWithGrouping(999n)).toBe("999");
  });

  it("inserts a separator every three digits beyond the first group", () => {
    expect(formatWithGrouping(1000n)).toBe("1,000");
    expect(formatWithGrouping(1_234_567n)).toBe("1,234,567");
  });
});

describe("shortAddr", () => {
  it("returns short strings unchanged at the threshold", () => {
    expect(shortAddr("abc")).toBe("abc");
    expect(shortAddr("12345678901")).toBe("12345678901");
  });

  it("truncates the middle past the threshold", () => {
    expect(shortAddr("ABCDEFGHIJKLMNOP")).toBe("ABCD…MNOP");
    expect(shortAddr("ABCDEFGHIJKLMNOP", 2, 2)).toBe("AB…OP");
  });
});

describe("tierLabel", () => {
  it("maps the four known lock tiers", () => {
    expect(tierLabel(5_000)).toBe("7 days · 0.5×");
    expect(tierLabel(10_000)).toBe("30 days · 1.0×");
    expect(tierLabel(15_000)).toBe("90 days · 1.5×");
    expect(tierLabel(20_000)).toBe("180 days · 2.0×");
  });

  it("falls back to a bare multiplier for unknown bps", () => {
    expect(tierLabel(7_500)).toBe("0.75×");
  });
});

describe("tierMultiplier", () => {
  it("renders bps as a 2-decimal multiplier", () => {
    expect(tierMultiplier(5_000)).toBe("0.50×");
    expect(tierMultiplier(10_000)).toBe("1.00×");
    expect(tierMultiplier(12_345)).toBe("1.23×");
  });
});

describe("formatPercent", () => {
  it("renders a 2-decimal percent by default", () => {
    expect(formatPercent(12.5)).toBe("12.50%");
    expect(formatPercent(0)).toBe("0.00%");
    expect(formatPercent(3.14159)).toBe("3.14%");
  });

  it("honors maxFrac", () => {
    expect(formatPercent(3.14159, { maxFrac: 4 })).toBe("3.1416%");
    expect(formatPercent(50, { maxFrac: 0 })).toBe("50%");
  });
});

describe("lockEndDate", () => {
  it("returns a dash for non-positive or non-finite timestamps", () => {
    expect(lockEndDate(0n)).toBe("—");
    expect(lockEndDate(-1n)).toBe("—");
  });

  it("renders a non-dash date for a positive timestamp", () => {
    const out = lockEndDate(1_700_000_000n);
    expect(out).not.toBe("—");
    expect(out.length).toBeGreaterThan(0);
  });
});

describe("trailingAnnualisedRate", () => {
  const NOW_SECONDS = 31_557_600;

  beforeEach(() => {
    vi.useFakeTimers();
    vi.setSystemTime(new Date(NOW_SECONDS * 1000));
  });

  afterEach(() => {
    vi.useRealTimers();
  });

  it("returns null with no elapsed time, no distributions, or no TVL", () => {
    expect(
      trailingAnnualisedRate({
        cumulativeSolLamports: 1_000_000_000n,
        initializedTs: BigInt(NOW_SECONDS) + 1n,
        totalWeightCvnt: 1_000_000n,
      }),
    ).toBeNull();
    expect(
      trailingAnnualisedRate({
        cumulativeSolLamports: 0n,
        initializedTs: 0n,
        totalWeightCvnt: 1_000_000n,
      }),
    ).toBeNull();
    expect(
      trailingAnnualisedRate({
        cumulativeSolLamports: 1_000_000_000n,
        initializedTs: 0n,
        totalWeightCvnt: 0n,
      }),
    ).toBeNull();
  });

  it("returns a CVNT-denominated rate when no price is supplied", () => {
    expect(
      trailingAnnualisedRate({
        cumulativeSolLamports: 1_000_000_000n,
        initializedTs: 0n,
        totalWeightCvnt: 1_000_000n,
      }),
    ).toBeCloseTo(1, 10);
  });

  it("returns a SOL-denominated percent when a price is supplied", () => {
    expect(
      trailingAnnualisedRate({
        cumulativeSolLamports: 1_000_000_000n,
        initializedTs: 0n,
        totalWeightCvnt: 1_000_000n,
        solPerCvnt: 1,
      }),
    ).toBeCloseTo(100, 10);
    expect(
      trailingAnnualisedRate({
        cumulativeSolLamports: 1_000_000_000n,
        initializedTs: 0n,
        totalWeightCvnt: 1_000_000n,
        solPerCvnt: 2,
      }),
    ).toBeCloseTo(50, 10);
  });
});

describe("relativeFromNow", () => {
  const NOW_SECONDS = 1_000_000_000;

  beforeEach(() => {
    vi.useFakeTimers();
    vi.setSystemTime(new Date(NOW_SECONDS * 1000));
  });

  afterEach(() => {
    vi.useRealTimers();
  });

  it("reports Unlocked at or past the target", () => {
    expect(relativeFromNow(BigInt(NOW_SECONDS))).toBe("Unlocked");
    expect(relativeFromNow(BigInt(NOW_SECONDS) - 10n)).toBe("Unlocked");
  });

  it("buckets remaining time into days, then hours, then minutes", () => {
    expect(relativeFromNow(BigInt(NOW_SECONDS) + 2n * 86_400n + 100n)).toBe("2d remaining");
    expect(relativeFromNow(BigInt(NOW_SECONDS) + 3n * 3_600n)).toBe("3h remaining");
    expect(relativeFromNow(BigInt(NOW_SECONDS) + 30n * 60n)).toBe("30m remaining");
  });
});
