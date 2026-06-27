import { describe, expect, it } from "vitest";
import { computeApy, formatApy, type ApyInput } from "../apy";

const base: ApyInput = {
  cumulativeSolDistributedLamports: 1_000_000_000n,
  initializedTsSeconds: 0n,
  nowSeconds: 31_536_000,
  lockedCvntRawAmount: 1_000_000n,
  solPerCvnt: 1,
};

describe("computeApy", () => {
  it("annualizes over a full elapsed year", () => {
    expect(computeApy(base)).toEqual({
      pct: 100,
      annualizedSol: 1,
      lockedSolValue: 1,
      elapsedDays: 365,
    });
  });

  it("scales annualizedSol by YEAR_SECONDS / elapsed", () => {
    const halfYear = computeApy({ ...base, nowSeconds: 15_768_000 });
    expect(halfYear).toEqual({
      pct: 200,
      annualizedSol: 2,
      lockedSolValue: 1,
      elapsedDays: 182.5,
    });
  });

  it("returns null below the minimum history window", () => {
    expect(computeApy({ ...base, nowSeconds: 599 })).toBeNull();
  });

  it("admits the boundary at exactly the minimum history window", () => {
    expect(computeApy({ ...base, nowSeconds: 600 })).not.toBeNull();
  });

  it("returns null when nothing is locked", () => {
    expect(computeApy({ ...base, lockedCvntRawAmount: 0n })).toBeNull();
  });

  it("returns null for a non-positive price", () => {
    expect(computeApy({ ...base, solPerCvnt: 0 })).toBeNull();
    expect(computeApy({ ...base, solPerCvnt: -1 })).toBeNull();
  });

  it("returns null for a non-finite price", () => {
    expect(computeApy({ ...base, solPerCvnt: Number.NaN })).toBeNull();
    expect(computeApy({ ...base, solPerCvnt: Number.POSITIVE_INFINITY })).toBeNull();
  });

  it("returns null when nothing has been distributed yet", () => {
    expect(computeApy({ ...base, cumulativeSolDistributedLamports: 0n })).toBeNull();
  });

  it("returns null when the locked value resolves non-positive", () => {
    expect(computeApy({ ...base, lockedCvntRawAmount: -1_000_000n })).toBeNull();
  });
});

describe("formatApy", () => {
  it("renders a placeholder for non-finite or negative input", () => {
    expect(formatApy(Number.NaN)).toBe("—");
    expect(formatApy(Number.POSITIVE_INFINITY)).toBe("—");
    expect(formatApy(-0.01)).toBe("—");
  });

  it("caps very large yields", () => {
    expect(formatApy(9999)).toBe("9999%+");
    expect(formatApy(50_000)).toBe("9999%+");
  });

  it("drops decimals at or above 100%", () => {
    expect(formatApy(100)).toBe("100%");
    expect(formatApy(149.7)).toBe("150%");
  });

  it("keeps one decimal in the 10-100% band", () => {
    expect(formatApy(10)).toBe("10.0%");
    expect(formatApy(12.34)).toBe("12.3%");
  });

  it("keeps two decimals below 10%", () => {
    expect(formatApy(0)).toBe("0.00%");
    expect(formatApy(5.678)).toBe("5.68%");
  });
});
