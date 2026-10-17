import React from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { afterEach, describe, expect, it, vi } from "vitest";
import {
  DAY, countLabel, fetchHistory, groupPoints, historyPath, price, recordingStart,
  samplingDescription, SOURCE_MARKET, SOURCE_QUOTE, SOURCE_SETTLED, sumSamples, validateHistory,
  type PriceHistory, type PricePoint, type SamplingStatus,
} from "../_history";
import { PriceChart, PriceHistoryView, PriceSeries } from "../PriceHistory";
import ComputePricesPage, { metadata } from "../page";

vi.mock("../../SiteHeader", () => ({ SiteHeader: () => <header>Shared site header</header> }));
vi.mock("../../SiteFooter", () => ({ SiteFooter: () => <footer>Shared site footer</footer> }));
const point = (patch: Partial<PricePoint> = {}): PricePoint => ({
  gpu_model: "L40S", provider: "vast.ai", source: SOURCE_MARKET, currency: "USD", day_start_ms: DAY,
  first_observed_at_ms: DAY + 1000, last_observed_at_ms: DAY + 2000, sample_count: 2,
  mean_micros_per_gpu_hour: 350000, min_micros_per_gpu_hour: 300000, max_micros_per_gpu_hour: 400000,
  proof_paths: [], ...patch,
});
const history = (points: PricePoint[] = []): PriceHistory => ({ version: 1, generated_at_ms: DAY + 3000, coordinator_pubkey_b58: "coordinator", points });
const render = (points: PricePoint[], currency: "USD" | "USDC" = "USD") => renderToStaticMarkup(<PriceSeries points={points} currency={currency} startMs={0} endMs={10 * DAY} />);
afterEach(() => { vi.unstubAllGlobals(); vi.restoreAllMocks(); });

describe("native Compute price page", () => {
  it("uses shared site layout and server-rendered metadata with the exact page title", () => {
    const html = renderToStaticMarkup(<ComputePricesPage />);
    expect(html).toContain("Shared site header"); expect(html).toContain("Shared site footer");
    expect(html).toMatch(/<h1[^>]*>GPU price history<\/h1>/);
    expect(html).toContain('class="page-container"'); expect(html).not.toContain("<iframe");
    expect(metadata.alternates?.canonical).toBe("https://opencovenant.org/compute-prices");
    expect(metadata.openGraph).toMatchObject({ images: [{ url: "https://opencovenant.org/og/compute-prices.png", width: 1200, height: 630 }] });
    expect(metadata.twitter).toMatchObject({ card: "summary_large_image" });
  });
  it("shows loading without inventing counts or chart observations on the server", () => {
    const html = renderToStaticMarkup(<PriceHistoryView />);
    expect(html).toContain("Loading price history"); expect(html).not.toContain("<circle");
    expect(html).not.toContain("No provider quotes in this view"); expect(html).toContain("access the price history as JSON");
  });
  it("retains exact millionth prices and singular sample labels", () => {
    for (const [value, expected] of [[0, "0.00"], [1, "0.000001"], [10000, "0.01"], [1200000, "1.20"], [1234567, "1.234567"]] as const) expect(price(value)).toBe(expected);
    expect(countLabel(1, "sample")).toBe("1 sample"); expect(countLabel(2, "sample")).toBe("2 samples");
  });
  it("keeps sources separate and escapes provider-supplied labels", () => {
    const points = [point({ gpu_model: "<img src=x onerror=alert(1)>" }), point({ source: SOURCE_QUOTE }), point({ source: SOURCE_SETTLED, currency: "USDC" })];
    expect(groupPoints(points)).toHaveLength(3);
    const html = render(points.slice(0, 2));
    expect(html).toContain("Market sample"); expect(html).toContain("Provisioned offer");
    expect(html).toContain("&lt;img"); expect(html).not.toContain("<img");
    expect(render([points[2]], "USDC")).toContain("USDC / GPU-hour");
  });
  it("keeps missing days empty without connected or zero-filled series", () => {
    const html = renderToStaticMarkup(<PriceChart points={[point(), point({ day_start_ms: 7 * DAY })]} currency="USD" startMs={0} endMs={10 * DAY} />);
    expect(html.match(/<circle/g)).toHaveLength(2); expect(html).not.toContain("<path"); expect(html).not.toContain("<polyline");
    expect(render([])).toContain("No provider quotes in this view"); expect(render([], "USDC")).toContain("No settled leases in this view");
  });
  it.each(["https://evil.test/proof", "//evil.test", "javascript:alert(1)", "/prices/proofs/../admin", "/proof/receipts/10000000-0000-0000-0000-000000000001", "/prices/proofs/10000000-0000-0000-0000-000000000001?secret=x"])("rejects unsupported proof route %s", path => {
    expect(() => validateHistory(history([point({ source: SOURCE_SETTLED, currency: "USDC", proof_paths: [path] })]))).toThrow("invalid observation");
  });
  it("renders unique same-origin proof links with exact daily values", () => {
    const path = "/prices/proofs/10000000-0000-0000-0000-000000000001";
    const p = point({ source: SOURCE_SETTLED, currency: "USDC", proof_paths: [path, path] });
    const html = render(validateHistory(history([p])).points, "USDC");
    expect(html.match(/href=/g)).toHaveLength(1); expect(html).toContain(`href="${path}"`); expect(html).toContain("0.35");
  });
  it("rejects wrong assets, unsafe numbers, invalid dates and contradictory price ranges", () => {
    for (const patch of [{ currency: "USDC" }, { sample_count: 0 }, { mean_micros_per_gpu_hour: 1.1 }, { mean_micros_per_gpu_hour: Number.MAX_SAFE_INTEGER + 1 }, { day_start_ms: 8_640_000_000_000_001 }, { min_micros_per_gpu_hour: 500000 }] as Partial<PricePoint>[]) expect(() => validateHistory(history([point(patch)]))).toThrow("invalid observation");
    expect(() => sumSamples([point({ sample_count: Number.MAX_SAFE_INTEGER }), point()])).toThrow("sample count");
  });
  it("distinguishes sampling failure from an empty capacity market", () => {
    const status: SamplingStatus = { identity: { provider: "vast.ai", gpu_models: ["L40S"], allocated_storage_gb: 16 }, state: "provider_unavailable", stale: true, persistence_healthy: true, last_success_at_ms: DAY, last_attempt_at_ms: DAY + 3000, last_sample_offer_count: 8, sample_interval_seconds: 3600, maximum_offers_per_model: 8 };
    const result = samplingDescription(status);
    expect(result.warning).toBe(true); expect(result.text).toContain("unavailable"); expect(result.text).toContain("does not mean provider capacity is empty");
    expect(result.text).toContain("16 GB of storage"); expect(result.text).toContain("8 offers per model across 1 GPU model");
  });
  it("uses the recorded start date only when the source provides a valid timestamp", () => {
    expect(recordingStart({ ...history(), settlement_identity: { recording_started_at_ms: 1792267615176 } })).toBe("2026-10-17");
    expect(recordingStart({ ...history(), settlement_identity: { recording_started_at_ms: "invalid" } })).toBeNull();
    expect(recordingStart(null)).toBeNull();
  });
  it("fetches the selected UTC window anonymously and rejects HTTP failures", async () => {
    const fetchMock = vi.fn().mockResolvedValue(new Response(JSON.stringify(history()), { status: 200 }));
    vi.stubGlobal("fetch", fetchMock);
    const path = historyPath(7, 10 * DAY); const controller = new AbortController();
    await expect(fetchHistory(path, controller.signal)).resolves.toMatchObject({ version: 1 });
    expect(path).toBe(`/prices/history?since_ms=${3 * DAY}&until_ms=${10 * DAY}`);
    expect(fetchMock).toHaveBeenCalledWith(path, expect.objectContaining({ credentials: "omit", cache: "no-store", signal: controller.signal }));
    fetchMock.mockResolvedValue(new Response("unavailable", { status: 503 }));
    await expect(fetchHistory(path, controller.signal)).rejects.toThrow("HTTP 503");
  });
});
