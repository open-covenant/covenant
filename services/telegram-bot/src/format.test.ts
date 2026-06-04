import { describe, expect, it } from "vitest";
import {
  fireBar,
  formatTokenAmount,
  logoBar,
  renderNewStake,
  solscanTxUrl,
  LOGO_BAR_MAX,
  type NewStakeMessage,
} from "./format.js";

const countFires = (s: string) => [...s].length;

describe("formatTokenAmount", () => {
  it("formats integer amounts with thousands grouping", () => {
    expect(formatTokenAmount(9_988_818_000_000n, 6, 0)).toBe("9,988,818");
  });

  it("formats two-decimal totals with grouping", () => {
    expect(formatTokenAmount(201_109_469_720_000n, 6, 2)).toBe(
      "201,109,469.72",
    );
  });

  it("rounds half-up across the decimal boundary", () => {
    expect(formatTokenAmount(9_999_000n, 6, 2)).toBe("10.00");
  });

  it("handles zero and sub-unit values", () => {
    expect(formatTokenAmount(0n, 6, 2)).toBe("0.00");
    expect(formatTokenAmount(1n, 6, 0)).toBe("0");
  });
});

describe("fireBar", () => {
  it("always shows at least one fire", () => {
    expect(countFires(fireBar(0n, 6, 250_000))).toBe(1);
  });

  it("scales one fire per unit", () => {
    expect(countFires(fireBar(1_000_000_000_000n, 6, 250_000))).toBe(4);
  });

  it("clamps to 50 fires", () => {
    expect(countFires(fireBar(99_000_000_000_000n, 6, 250_000))).toBe(50);
  });
});

describe("solscanTxUrl", () => {
  it("omits the cluster query on mainnet", () => {
    expect(solscanTxUrl("https://solscan.io", "abc", "mainnet-beta")).toBe(
      "https://solscan.io/tx/abc",
    );
  });

  it("appends the cluster query off mainnet and trims a trailing slash", () => {
    expect(solscanTxUrl("https://solscan.io/", "abc", "devnet")).toBe(
      "https://solscan.io/tx/abc?cluster=devnet",
    );
  });
});

const base: NewStakeMessage = {
  amountRaw: 9_988_818_000_000n,
  decimals: 6,
  multiplierBps: 5000,
  totals: { totalStakedRaw: 201_109_469_720_000n, pct: 20.1 },
  txSignature: "SiG",
  cluster: "mainnet-beta",
  symbol: "CVNT",
  stakeUrl: "https://opencovenant.org/stake",
  solscanBase: "https://solscan.io",
  fireUnit: 250_000,
};

describe("renderNewStake", () => {
  it("renders the full announcement", () => {
    const html = renderNewStake(base);
    expect(html).toContain("<b>NEW STAKE</b>");
    expect(html).toContain("9,988,818 $CVNT · 7d lock");
    expect(html).toContain("Total staked: 201,109,469.72 $CVNT");
    expect(html).toContain("20.1% of supply staked");
    expect(html).toContain(
      '<a href="https://solscan.io/tx/SiG">View on Solscan</a>',
    );
    expect(html).toContain("Stake $CVNT →</a>");
    expect(html).toContain("🔥");
  });

  it("omits the totals lines when totals is null", () => {
    const html = renderNewStake({ ...base, totals: null });
    expect(html).not.toContain("Total staked");
    expect(html).not.toContain("of supply staked");
    expect(html).toContain("9,988,818 $CVNT · 7d lock");
  });

  it("escapes HTML metacharacters in the symbol", () => {
    const html = renderNewStake({ ...base, symbol: "C<V>T" });
    expect(html).toContain("$C&lt;V&gt;T");
    expect(html).not.toContain("$C<V>T");
  });

  it("labels each lock tier", () => {
    expect(renderNewStake({ ...base, multiplierBps: 10_000 })).toContain(
      "· 30d lock",
    );
    expect(renderNewStake({ ...base, multiplierBps: 15_000 })).toContain(
      "· 90d lock",
    );
    expect(renderNewStake({ ...base, multiplierBps: 20_000 })).toContain(
      "· 180d lock",
    );
  });

  it("uses the branded custom-emoji bar when emojiId is set (no 🔥)", () => {
    const html = renderNewStake({ ...base, emojiId: "5841217997154295453" });
    expect(html).toContain('<tg-emoji emoji-id="5841217997154295453">🔥</tg-emoji>');
    // a real-but-capped count, space-separated, and no raw fire emoji left
    expect(html).toContain("</tg-emoji> <tg-emoji");
    expect(html).not.toContain("🔥🔥");
  });

  it("drops the NEW STAKE title in bannerMode but keeps the bar + body", () => {
    const html = renderNewStake({
      ...base,
      bannerMode: true,
      emojiId: "5841217997154295453",
    });
    expect(html).not.toContain("NEW STAKE");
    expect(html.startsWith("<tg-emoji")).toBe(true); // caption leads with the bar
    expect(html).toContain("9,988,818 $CVNT · 7d lock");
    expect(html).toContain("Total staked: 201,109,469.72 $CVNT");
  });
});

describe("logoBar", () => {
  const id = "5841217997154295453";
  const count = (s: string) => (s.match(/<tg-emoji /g) ?? []).length;

  it("emits space-separated custom emoji", () => {
    const bar = logoBar(1_000_000_000_000n, 6, 250_000, id); // 1,000,000/250,000 = 4
    expect(count(bar)).toBe(4);
    expect(bar).toContain("</tg-emoji> <tg-emoji "); // a space between each
  });

  it("caps at LOGO_BAR_MAX", () => {
    expect(count(logoBar(99_000_000_000_000n, 6, 250_000, id))).toBe(LOGO_BAR_MAX);
  });

  it("always shows at least one", () => {
    expect(count(logoBar(0n, 6, 250_000, id))).toBe(1);
  });
});
