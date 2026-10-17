import React from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it, vi } from "vitest";
import ComputeStatusPage, { metadata } from "../page";
import { ComputeStatusView, StatusReport } from "../ComputeStatus";
import { fetchStatus, observerState, snapshotCurrent, STATUS_MAX_BYTES, validateStatus, type ComputeStatus, type ObserverState } from "../_status";
import { proxyStatus } from "../_proxy";

vi.mock("../../SiteHeader", () => ({ SiteHeader: () => <header>Shared site header</header> }));
vi.mock("../../SiteFooter", () => ({ SiteFooter: () => <footer>Shared site footer</footer> }));
const NOW = 1_792_270_000_000;
const fixture = (): ComputeStatus => ({
  schema_version: 1, generated_at_ms: NOW, mode: "alert_only", journal_writable: true, journal_persistence: "durable",
  chain_observer: { status: "healthy", observed_at_ms: NOW - 1000, l1_slot: 12345, scan_complete: true, scanned_leases: 2, finding_counts: {}, coverage: "leases_observed", stale_after_ms: 180000 },
  capacity: { registered_operators: 6, matchable_operators: 2, entries: [{ kind: "lease_session", model: "any", operators: 2 }] },
  latest_recorded_lease_payout_at_ms: null, incidents: { schema_version: 1, incidents: [] },
  deployment: { revision: "abc123", coordinator: "coordinator", chain: "solana", genesis_hash: "genesis", program: "program", mint: "mint", lease_coordinator: "lease-coordinator", funding_renter: "service-funding", validator: "validator", observer_configured: true },
  alerts: { confirmation_observations: 3, active_count: 0, pending_delivery_count: 0, delivery: "structured_logs", persistence_healthy: true, last_delivery_at_ms: null },
});
const jsonResponse = (value: unknown = fixture(), init: ResponseInit = {}) => new Response(JSON.stringify(value), { ...init, headers: { "content-type": "application/json", "cache-control": "no-store", ...init.headers } });
const render = (data = fixture(), nowMs = NOW, requestFailed = false) => renderToStaticMarkup(<StatusReport data={data} nowMs={nowMs} requestFailed={requestFailed} />);

describe("public Compute status page", () => {
  it("uses the shared layout with canonical and generated large-image metadata", () => {
    const html = renderToStaticMarkup(<ComputeStatusPage />);
    expect(html).toContain("Shared site header"); expect(html).toContain("Shared site footer");
    expect(html).toContain('class="page-container"'); expect(html).toMatch(/<h1[^>]*>Compute status<\/h1>/);
    expect(metadata.alternates?.canonical).toBe("https://opencovenant.org/compute-status");
    expect(metadata.openGraph).toMatchObject({ images: [{ url: "https://opencovenant.org/og/compute-status.png", width: 1200, height: 630 }] });
    expect(metadata.twitter).toMatchObject({ card: "summary_large_image" });
  });
  it("does not invent service health or zero counts before the first response", () => {
    const html = renderToStaticMarkup(<ComputeStatusView />);
    expect(html).toContain("Loading status report"); expect(html).toContain("read the status report as JSON");
    expect(html).not.toContain("No discrepancies detected"); expect(html).not.toContain("0 eligible operators");
  });
  it("does not equate empty capacity, an empty scan or unpublished incidents with verified health", () => {
    const data = fixture();
    data.capacity = { registered_operators: 6, matchable_operators: 0, entries: [] };
    data.chain_observer.scanned_leases = 0; data.chain_observer.coverage = "no_leases";
    const html = render(validateStatus(data));
    expect(html).toContain("Empty capacity alone does not establish a service outage");
    expect(html).toContain("provides no evidence of a completed rental or settlement");
    expect(html).toContain("No reviewed incidents have been published"); expect(html).not.toContain("All systems operational");
  });
  it.each([
    ["disabled", "Not enabled"], ["not_observed", "Awaiting first scan"], ["healthy", "No discrepancies detected"],
    ["degraded", "Discrepancies detected"], ["blind", "Chain visibility incomplete"], ["stale", "Observation overdue"],
  ] as [ObserverState, string][])("distinguishes the %s observation state", (state, label) => {
    const data = fixture(); data.chain_observer.status = state;
    expect(render(data)).toContain(label);
  });
  it("ages snapshots and observations independently, and removes healthy styling after a failed refresh", () => {
    const data = fixture();
    expect(snapshotCurrent(data, NOW + 180001)).toBe(false);
    expect(snapshotCurrent(data, NOW - 30001)).toBe(false);
    expect(snapshotCurrent(data, NOW, true)).toBe(false);
    data.chain_observer.stale_after_ms = 30000;
    expect(observerState(data, NOW + 30000)).toBe("stale");
    expect(snapshotCurrent(data, NOW + 30000)).toBe(true);
    const failed = render(data, NOW, true);
    expect(failed).toContain("Current status is unknown"); expect(failed).toContain("The latest refresh failed");
    expect(failed).toContain("at last report"); expect(failed).not.toContain("text-ok");
    expect(render(data, NOW + 180001)).toContain("outside the freshness window");
  });
  it("distinguishes failed persistence and in-memory records from a durable writable journal", () => {
    const data = fixture(); data.journal_persistence = "memory";
    expect(render(data)).toContain("Memory only"); expect(render(data)).not.toContain("durable journal can accept writes");
    data.journal_writable = false; data.alerts!.persistence_healthy = false; data.alerts!.pending_delivery_count = 2;
    const html = render(data);
    expect(html).toContain("Writes unavailable"); expect(html).toContain("Alert persistence needs attention"); expect(html).toContain("2 alert deliveries are pending");
  });
  it("renders reviewed chronology and treats incident text as plain text", () => {
    const data = fixture();
    data.incidents.incidents = [{ id: "incident-1", started_at_ms: NOW - 2000, resolved_at_ms: NOW - 1000, effect: "<img src=x onerror=alert(1)>", cause: "RPC unavailable", fix: "Connectivity restored", updates: [{ at_ms: NOW - 2000, message: "Investigating" }, { at_ms: NOW - 1000, message: "Resolved" }] }];
    const html = render(validateStatus(data));
    expect(html).toContain("&lt;img"); expect(html).not.toContain("<img"); expect(html).toContain("Connectivity restored");
    data.incidents.incidents[0].updates.reverse(); expect(() => validateStatus(data)).toThrow("invalid report");
  });
});

describe("public status validation and fetch", () => {
  it("rejects contradictory health, invalid numbers and unrecognized contracts", () => {
    const cases: (() => ComputeStatus)[] = [
      () => ({ ...fixture(), schema_version: 2 as 1 }),
      () => ({ ...fixture(), generated_at_ms: Number.MAX_SAFE_INTEGER + 1 }),
      () => ({ ...fixture(), journal_writable: "yes" as unknown as boolean }),
      () => ({ ...fixture(), chain_observer: { ...fixture().chain_observer, scan_complete: false } }),
      () => ({ ...fixture(), chain_observer: { ...fixture().chain_observer, finding_counts: { settlement_mismatch: 1 } } }),
      () => ({ ...fixture(), chain_observer: { ...fixture().chain_observer, status: "disabled", observed_at_ms: NOW } }),
      () => ({ ...fixture(), chain_observer: { ...fixture().chain_observer, coverage: "no_leases" } }),
      () => ({ ...fixture(), capacity: { ...fixture().capacity, matchable_operators: 7 } }),
      () => ({ ...fixture(), capacity: { ...fixture().capacity, entries: [{ kind: "lease_session", model: "any", operators: 3 }] } }),
      () => ({ ...fixture(), latest_recorded_lease_payout_at_ms: NOW + 1 }),
    ];
    for (const bad of cases) expect(() => validateStatus(bad())).toThrow("invalid report");
  });
  it("discards unknown private details instead of reflecting them into the proxy response", () => {
    const data = { ...fixture(), rpc_url: "https://example.test/secret", jobs: [{ buyer: "private" }], chain_observer: { ...fixture().chain_observer, findings: [{ account: "private" }] } };
    const parsed = JSON.stringify(validateStatus(data));
    expect(parsed).not.toContain("secret"); expect(parsed).not.toContain("private"); expect(parsed).not.toContain("rpc_url");
  });
  it("accepts a disabled deployment with no alert configuration and a configured email destination", () => {
    const data = fixture();
    data.chain_observer = { ...data.chain_observer, status: "disabled", observed_at_ms: null, l1_slot: null, scan_complete: false, scanned_leases: 0, coverage: "not_observed" };
    data.alerts = null; data.deployment.observer_configured = false;
    expect(render(validateStatus(data))).toContain("Observer alert delivery is not enabled");
    const enabled = fixture(); enabled.alerts!.delivery = "email";
    expect(render(validateStatus(enabled))).toContain("configured email destination");
  });
  it("fetches only the same-origin endpoint without credentials or cache, preserving aborts", async () => {
    const fetcher = vi.fn().mockResolvedValue(jsonResponse()); const controller = new AbortController();
    await expect(fetchStatus(controller.signal, fetcher)).resolves.toMatchObject({ mode: "alert_only" });
    expect(fetcher).toHaveBeenCalledWith("/compute-status/data", expect.objectContaining({ credentials: "omit", cache: "no-store", redirect: "error", signal: controller.signal }));
    fetcher.mockResolvedValue(new Response("<h1>Unavailable</h1>", { status: 503 }));
    await expect(fetchStatus(controller.signal, fetcher)).rejects.toThrow("HTTP 503");
    fetcher.mockResolvedValue(new Response("{}", { headers: { "content-type": "text/html" } }));
    await expect(fetchStatus(controller.signal, fetcher)).rejects.toThrow("invalid report");
    fetcher.mockRejectedValue(new DOMException("Aborted", "AbortError"));
    await expect(fetchStatus(controller.signal, fetcher)).rejects.toMatchObject({ name: "AbortError" });
  });
});

describe("bounded status proxy", () => {
  it("uses only the public fixed origin, accepts no-store JSON and returns private no-store data", async () => {
    const fetcher = vi.fn().mockResolvedValue(jsonResponse());
    const response = await proxyStatus(new URLSearchParams(), fetcher);
    expect(response.status).toBe(200); expect(response.headers.get("cache-control")).toBe("private, no-store, max-age=0");
    expect(await response.json()).toEqual(fixture());
    expect(fetcher).toHaveBeenCalledWith("https://compute-api.opencovenant.org/status", expect.objectContaining({ credentials: "omit", redirect: "manual", cache: "no-store", headers: { accept: "application/json" }, signal: expect.any(AbortSignal) }));
  });
  it("rejects arbitrary queries without contacting upstream", async () => {
    const fetcher = vi.fn(); const response = await proxyStatus(new URLSearchParams("url=https://example.test"), fetcher);
    expect(response.status).toBe(400); expect(fetcher).not.toHaveBeenCalled(); expect(response.headers.get("cache-control")).toContain("no-store");
  });
  it.each([
    () => new Response(null, { status: 302, headers: { location: "https://example.test/private" } }),
    () => new Response("private error details", { status: 500 }),
    () => jsonResponse(fixture(), { headers: { "set-cookie": "secret=123" } }),
    () => new Response("<html>login</html>", { headers: { "content-type": "text/html" } }),
    () => jsonResponse({ schema_version: 99 }),
    () => jsonResponse(fixture(), { headers: { "content-length": String(STATUS_MAX_BYTES + 1) } }),
    () => new Response(" ".repeat(STATUS_MAX_BYTES + 1), { headers: { "content-type": "application/json" } }),
  ])("returns a generic unavailable state for rejected responses", async factory => {
    const response = await proxyStatus(new URLSearchParams(), vi.fn().mockResolvedValue(factory()));
    expect(response.status).toBe(503); expect(response.headers.get("cache-control")).toContain("no-store");
    expect(await response.json()).toEqual({ error: "Status data temporarily unavailable" });
  });
  it("does not expose network exception details", async () => {
    const response = await proxyStatus(new URLSearchParams(), vi.fn().mockRejectedValue(new Error("secret RPC credentials")));
    expect(response.status).toBe(503); expect(await response.text()).not.toContain("secret");
  });
});
