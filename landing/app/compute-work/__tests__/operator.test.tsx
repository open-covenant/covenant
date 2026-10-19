import React from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it, vi } from "vitest";
import { OperatorStatusView } from "../OperatorStatus";
import { liquidityReady, OPERATOR_FRESH_MS, READINESS_IDS, validateOperatorSnapshot, type OperatorSnapshot } from "../_operator";
import { proxyOperator } from "../_operator-proxy";

const NOW = 1_792_396_800_000;
const HASH = "a".repeat(64);
function fixture(): OperatorSnapshot {
  return {
    schema_version: 1, generated_at_ms: NOW, pool_funded: false, stage: "preparing", quote_observation: null,
    readiness: READINESS_IDS.map(id => ({ id, status: "pending", checked_at_ms: null })),
    analyst: { status: "pending", evidence_manifest_url: null, source_sha256: null },
    costs: { reserved_usdc_micros: "1500000", coordinator_reported_charge_usdc_micros: "0", settlement: "unresolved" },
  };
}
function ready(): OperatorSnapshot {
  const data = fixture();
  data.stage = "awaiting_liquidity";
  data.readiness = READINESS_IDS.map(id => ({ id, status: "passed", checked_at_ms: NOW }));
  data.analyst = { status: "accepted", evidence_manifest_url: `/pool-operator/manifests/${HASH}.json`, source_sha256: HASH };
  data.quote_observation = { observed_at_ms: NOW - 10_000, completed_at_ms: NOW, status: "available", available_quotes: 6,
    observation_sha256: HASH, records_url: `/pool-operator/observations/${HASH}.json` };
  return data;
}
const html = (data: OperatorSnapshot | null, nowMs = NOW, failed = false) => renderToStaticMarkup(<OperatorStatusView snapshot={data} nowMs={nowMs} failed={failed} />);
const response = (value: unknown, headers: Record<string, string> = {}) => new Response(JSON.stringify(value), { headers: { "content-type": "application/json", ...headers } });

describe("public operator schema and readiness", () => {
  it("reconstructs public fields and strips private fields at every depth", () => {
    const source = { ...ready(), buyer_key: "secret", readiness: ready().readiness.map(g => ({ ...g, internal_job: "secret" })),
      costs: { ...ready().costs, receipt_token: "secret" }, analyst: { ...ready().analyst, signed_request: "secret" } };
    const result = validateOperatorSnapshot(source);
    expect(JSON.stringify(result)).not.toContain("secret");
    expect(result.analyst.evidence_manifest_url).toBe(`https://compute-api.opencovenant.org/pool-operator/manifests/${HASH}.json`);
    expect(liquidityReady(result, NOW)).toBe(true);
  });
  it("requires every gate, including the seven-day shadow trial", () => {
    const data = ready(); data.readiness.find(g => g.id === "shadow_trial")!.status = "pending";
    expect(() => validateOperatorSnapshot(data)).toThrow();
    data.stage = "observing";
    expect(liquidityReady(validateOperatorSnapshot(data), NOW)).toBe(false);
    expect(html(data)).toContain("Seven-day observation");
  });
  it("does not refresh an old observation by republishing the report", () => {
    const data = ready(); data.quote_observation!.observed_at_ms = NOW - OPERATOR_FRESH_MS - 1;
    expect(liquidityReady(validateOperatorSnapshot(data), NOW)).toBe(false);
    expect(html(data)).toContain("Observation outdated");
    expect(html(data)).not.toContain(">Awaiting liquidity<");
  });
  it("revokes current readiness on stale report or fetch failure", () => {
    expect(liquidityReady(ready(), NOW + OPERATOR_FRESH_MS + 1)).toBe(false);
    expect(liquidityReady(ready(), NOW, true)).toBe(false);
    expect(html(ready(), NOW, true)).toContain("Status is outdated");
    expect(html(null, NOW, true)).toContain("Readiness has not been confirmed");
    expect(html(ready(), NOW, true)).not.toContain(">Awaiting liquidity<");
  });
  it("rejects funded state, missing or duplicate gates, unsupported stages and incomplete accepted evidence", () => {
    for (const mutate of [
      (d: any) => { d.pool_funded = true; }, (d: any) => { d.readiness.pop(); },
      (d: any) => { d.readiness[4] = d.readiness[0]; }, (d: any) => { d.stage = "live"; },
      (d: any) => { d.analyst.evidence_manifest_url = null; }, (d: any) => { d.analyst.source_sha256 = null; },
      (d: any) => { d.readiness[0].checked_at_ms = null; }, (d: any) => { d.generated_at_ms = true; },
    ]) { const data = ready(); mutate(data); expect(() => validateOperatorSnapshot(data)).toThrow(); }
  });
  it("rejects private, redirected, noncanonical and hash-mismatched artifact links", () => {
    for (const url of ["https://other.test/pool-operator/manifests/" + HASH + ".json", "/jobs/private", "/pool-operator/manifests/" + HASH + ".json?token=secret",
      "https://user@compute-api.opencovenant.org/pool-operator/manifests/" + HASH + ".json", "//compute-api.opencovenant.org/pool-operator/manifests/" + HASH + ".json"]) {
      const data = ready(); data.analyst.evidence_manifest_url = url; expect(() => validateOperatorSnapshot(data)).toThrow();
    }
    const data = ready(); data.quote_observation!.records_url = `/pool-operator/observations/${"b".repeat(64)}.json`;
    expect(() => validateOperatorSnapshot(data)).toThrow();
  });
  it("rejects contradictory observations and noncanonical costs", () => {
    for (const mutate of [
      (d: any) => { d.quote_observation.available_quotes = 5; }, (d: any) => { d.quote_observation.observed_at_ms = NOW + 1; },
      (d: any) => { d.quote_observation.completed_at_ms = NOW + 1; }, (d: any) => { d.costs.reserved_usdc_micros = "01"; },
      (d: any) => { d.costs.reserved_usdc_micros = 1; }, (d: any) => { d.costs.reserved_usdc_micros = "1000000001"; },
      (d: any) => { d.costs.settlement = "reconciled"; d.costs.coordinator_reported_charge_usdc_micros = null; },
    ]) { const data = ready(); mutate(data); expect(() => validateOperatorSnapshot(data)).toThrow(); }
  });
  it("labels quotes as observations and a zero reported charge as unresolved", () => {
    const rendered = html(validateOperatorSnapshot(ready()));
    expect(rendered).toContain("6/6 external quotes available");
    expect(rendered).toContain("do not establish trading demand, fills, earned fees or profit");
    expect(rendered).toContain("1.50 USDC");
    expect(rendered).toContain("A reported zero charge does not release a reservation");
    expect(rendered).toContain("pool is still unfunded");
  });
  it("retains accepted evidence in the live view without making an unfunded claim", () => {
    const rendered = renderToStaticMarkup(<OperatorStatusView snapshot={validateOperatorSnapshot(ready())} nowMs={NOW} archive />);
    expect(rendered).toContain("Preparation evidence");
    expect(rendered).toContain("Inspect the accepted analyst evidence");
    expect(rendered).toContain("Inspect this observation");
    expect(rendered).not.toContain("Unfunded pilot");
    expect(rendered).not.toContain("Awaiting liquidity");
    expect(rendered).not.toContain("pool is still unfunded");
  });
});

describe("operator public proxy", () => {
  it("reads only the fixed public API without credentials and strips unrecognized private fields", async () => {
    const fetcher = vi.fn(async () => response({ ...fixture(), bearer: "secret" }));
    const result = await proxyOperator(new URLSearchParams(), fetcher as typeof fetch);
    expect(result.status).toBe(200);
    expect(await result.text()).not.toContain("secret");
    expect(fetcher).toHaveBeenCalledWith("https://compute-api.opencovenant.org/pool-operator", expect.objectContaining({ credentials: "omit", redirect: "manual", cache: "no-store" }));
    expect(result.headers.get("cache-control")).toContain("no-store");
  });
  it("rejects query parameters without making a request", async () => {
    const fetcher = vi.fn(); expect((await proxyOperator(new URLSearchParams("url=https://private.test"), fetcher)).status).toBe(400);
    expect(fetcher).not.toHaveBeenCalled();
  });
  it("fails closed on redirect, cookies, malformed/oversized JSON and private errors", async () => {
    for (const upstream of [new Response("private", { status: 302, headers: { location: "https://private.test" } }),
      response(fixture(), { "set-cookie": "secret" }), new Response("secret", { headers: { "content-type": "text/html" } }),
      response({ ...fixture(), schema_version: 2 }), new Response("x".repeat(65537), { headers: { "content-type": "application/json" } })]) {
      const result = await proxyOperator(new URLSearchParams(), vi.fn(async () => upstream) as typeof fetch);
      expect(result.status).toBe(503); expect(await result.text()).toBe('{"error":"Public pilot status temporarily unavailable"}');
    }
    const result = await proxyOperator(new URLSearchParams(), vi.fn(async () => { throw new Error("private auth secret"); }) as typeof fetch);
    expect(await result.text()).not.toContain("secret");
  });
});
