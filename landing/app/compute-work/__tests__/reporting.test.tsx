import React from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";
import { WorkReport } from "../ComputeWork";
import { validateSnapshot, WORK_FRESH_MS, type Pool, type WorkSnapshot } from "../_work";

const NOW = 1_792_370_000_000;
const pool = (): Pool => ({
  address: "1".repeat(32), position_address: "2".repeat(32), active: true,
  observed_at_ms: NOW, finalized_slot: 1000, fee_bps: 30, range: "full",
  cvnt_raw: "150000000", nvdax_raw: "400000000", nvdax_multiplier: "1.25",
  cvnt_fees_owed_raw: "30", nvdax_fees_owed_raw: "40",
  volume_usdc_micros: "123456789", lp_value_usdc_micros: "901234567", hold_value_usdc_micros: "910000000",
  market: { source: "orca_public_api", observed_at_ms: NOW, source_updated_at_ms: NOW - 1000, source_updated_slot: 999, window_seconds: 86400 },
  valuation: { source: "operator_reviewed_marks", observed_at_ms: NOW, baseline_at_ms: NOW - 86_400_000, baseline_slot: 100,
    baseline_sha256: "a".repeat(64), marks_sha256: "b".repeat(64), price_basis: "unscaled_token", cvnt_price_usdc: "0.01234567", nvdax_price_usdc: "215.12345678" },
});
const snapshot = (position: Pool = pool()): WorkSnapshot => ({
  schema_version: 1, generated_at_ms: NOW, mode: "rehearsal", pool: position,
  fees: { cvnt_retained_raw: "0", nvdax_collected_raw: "0", nvdax_converted_raw: "0" },
  budget: { available_usdc_micros: "0", reserved_usdc_micros: "0", spent_usdc_micros: "0", subsidy_usdc_micros: "0" }, work: [],
});
const render = (data: WorkSnapshot, now = NOW, failed = false) => renderToStaticMarkup(<WorkReport data={data} nowMs={now} requestFailed={failed} />);

describe("pool reporting provenance", () => {
  it("accepts indexed volume and reviewed inventory values with independent references", () => {
    const data = validateSnapshot(snapshot());
    expect(data).toEqual(snapshot());
    const html = render(data);
    expect(html).toContain("123.456789 USDC");
    expect(html).toContain("901.234567 USDC");
    expect(html).toContain("910.00 USDC");
    expect(html).toContain("Orca indexer estimate for this pool");
    expect(html).toContain("Values use operator-reviewed prices");
    expect(html).toContain("does not show total profit or loss");
    expect(html).not.toContain("current trading activity is unknown");
  });

  it("accepts older reports with absent optional metadata only when all metrics are unknown", () => {
    const position = pool();
    delete position.market; delete position.valuation;
    position.volume_usdc_micros = position.lp_value_usdc_micros = position.hold_value_usdc_micros = null;
    const result = validateSnapshot(snapshot(position));
    expect(result.pool?.market).toBeNull(); expect(result.pool?.valuation).toBeNull();
    expect(render(result)).toContain("No current trading-volume report is available");
    expect(render(result)).toContain("The value comparison needs current reviewed prices");
    expect(render(result)).not.toContain("Orca indexer estimate");
  });

  it("keeps an observed zero volume distinct from an absent observation", () => {
    const zero = pool(); zero.volume_usdc_micros = "0";
    const observed = validateSnapshot(snapshot(zero));
    expect(observed.pool?.volume_usdc_micros).toBe("0");
    expect(render(observed)).toContain("Orca indexer estimate for this pool");
    expect(render(observed)).not.toContain("No current trading-volume report is available");
    const missing = pool(); missing.volume_usdc_micros = null; missing.market = null;
    expect(render(validateSnapshot(snapshot(missing)))).toContain("That does not mean no trades occurred");
  });

  it.each([
    { volume_usdc_micros: null }, { market: null }, { market: undefined },
    { lp_value_usdc_micros: null }, { hold_value_usdc_micros: null },
    { valuation: null }, { valuation: undefined },
  ])("rejects values and provenance that disagree: %j", change => {
    expect(() => validateSnapshot(snapshot({ ...pool(), ...change }))).toThrow();
  });

  it("strips private nested data rather than forwarding indexer or pricing payloads", () => {
    const position = pool();
    const report = { ...snapshot(), pool: { ...position, wallet: "private-wallet",
      market: { ...position.market, api_key: "private-indexer-key", trades: [{ buyer: "private-buyer" }] },
      valuation: { ...position.valuation, approver_email: "private-operator", raw_marks: { credentials: "private-secret" } },
    } };
    const publicData = validateSnapshot(report);
    expect(publicData).toEqual(snapshot());
    expect(JSON.stringify(publicData)).not.toContain("private-");
  });

  it.each([
    { source: "untrusted_indexer" }, { window_seconds: 3600 }, { source_updated_slot: 0 },
    { source_updated_slot: 1001 }, { source_updated_slot: 1.5 },
    { observed_at_ms: NOW + 1 }, { source_updated_at_ms: NOW + 1 },
    { observed_at_ms: NOW - 2000, source_updated_at_ms: NOW - 1000 },
    { source_updated_at_ms: NOW - WORK_FRESH_MS - 1 },
  ])("rejects invalid, future or already stale market provenance: %j", change => {
    const report = snapshot();
    const changed = { ...report, pool: { ...report.pool, market: { ...report.pool?.market, ...change } } };
    expect(() => validateSnapshot(changed)).toThrow();
  });

  it.each([
    { source: "unreviewed_prices" }, { price_basis: "display_token" },
    { observed_at_ms: NOW + 1 }, { observed_at_ms: NOW - WORK_FRESH_MS - 1 },
    { baseline_at_ms: NOW + 1 }, { baseline_slot: 0 }, { baseline_slot: 1001 },
    { baseline_sha256: "A".repeat(64) }, { marks_sha256: "b".repeat(63) },
    { cvnt_price_usdc: "0" }, { cvnt_price_usdc: "-1" }, { cvnt_price_usdc: 0.1 },
    { nvdax_price_usdc: "1e2" }, { nvdax_price_usdc: "01" }, { nvdax_price_usdc: "Infinity" },
  ])("rejects invalid, future or already stale valuation provenance: %j", change => {
    const report = snapshot();
    const changed = { ...report, pool: { ...report.pool, valuation: { ...report.pool?.valuation, ...change } } };
    expect(() => validateSnapshot(changed)).toThrow();
  });

  it("requires a finalized position observation for indexed and reviewed references", () => {
    expect(() => validateSnapshot(snapshot({ ...pool(), finalized_slot: null }))).toThrow();
  });

  it("accepts observations at the freshness boundary without changing their timestamps", () => {
    const position = pool();
    position.market!.source_updated_at_ms = NOW - WORK_FRESH_MS;
    position.valuation!.observed_at_ms = NOW - WORK_FRESH_MS;
    const result = validateSnapshot(snapshot(position));
    expect(result.pool?.market?.source_updated_at_ms).toBe(NOW - WORK_FRESH_MS);
    expect(result.pool?.valuation?.observed_at_ms).toBe(NOW - WORK_FRESH_MS);
  });
});

describe("pool reporting presentation", () => {
  it("ages indexed volume independently while the snapshot, position and prices remain fresh", () => {
    const position = pool(); position.market!.source_updated_at_ms = NOW - 170_000;
    const html = render(validateSnapshot(snapshot(position)), NOW + 20_000);
    expect(html).toContain("Volume observation stale; current trading activity is unknown");
    expect(html).toContain("123.456789 USDC · last report");
    expect(html).not.toContain("Price observation stale");
    expect(html).not.toContain("Current funding state is unknown");
    expect(html).not.toContain("0.30% observed fee · observation stale");
  });

  it("ages reviewed prices independently while current volume and the funding snapshot remain fresh", () => {
    const position = pool(); position.valuation!.observed_at_ms = NOW - 170_000;
    const html = render(validateSnapshot(snapshot(position)), NOW + 20_000);
    expect(html).toContain("Price observation stale; current values are unknown");
    expect(html).toContain("901.234567 USDC · last report");
    expect(html).toContain("910.00 USDC · last report");
    expect(html).not.toContain("Volume observation stale");
    expect(html).not.toContain("Current funding state is unknown");
  });

  it("marks both observation sources unknown after a failed refresh", () => {
    const html = render(validateSnapshot(snapshot()), NOW, true);
    expect(html).toContain("Current funding state is unknown");
    expect(html).toContain("Volume observation stale; current trading activity is unknown");
    expect(html).toContain("Price observation stale; current values are unknown");
  });

  it("preserves raw quantities, unscaled price marks and exact large-value amounts", () => {
    const position = pool();
    position.cvnt_raw = "900719925474099312345678";
    position.volume_usdc_micros = "900719925474099312345678";
    position.valuation!.cvnt_price_usdc = "0.000000000000000000123456789012345678";
    const data = validateSnapshot(snapshot(position)), html = render(data);
    expect(html).toContain("900,719,925,474,099,312,345,678 raw units");
    expect(html).toContain("900,719,925,474,099,312.345678 USDC");
    expect(html).toContain("0.000000000000000000123456789012345678 USDC");
    expect(html).toContain("NVDAx price per unscaled token");
    expect(html).toContain("215.12345678 USDC");
    expect(html).toContain("display multiplier 1.25");
    position.nvdax_multiplier = "2.5";
    const changed = render(validateSnapshot(snapshot(position)));
    expect(changed).toContain("215.12345678 USDC");
    expect(changed).toContain("901.234567 USDC");
    expect(changed).toContain("display multiplier 2.5");
  });
});
