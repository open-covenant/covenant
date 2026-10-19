import React from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it, vi } from "vitest";
import ComputeWorkPage, { metadata } from "../page";
import { ComputeWorkView, WorkReport } from "../ComputeWork";
import { WorkArtifact } from "../WorkArtifact";
import { proxyWork } from "../_proxy";
import { cardHeadline, fetchSnapshot, fullyFeeFunded, snapshotCurrent, usdc, validateDetail, validateManifest, validateSnapshot, WORK_MAX_BYTES, type Work, type WorkSnapshot } from "../_work";

vi.mock("../../SiteHeader", () => ({ SiteHeader: () => <header>Shared site header</header> }));
vi.mock("../../SiteFooter", () => ({ SiteFooter: () => <footer>Shared site footer</footer> }));
const NOW = 1_792_370_000_000;
const work = (): Work => ({
  id: "payfetch-v2-header", title: "Payfetch v2 payment header", summary: "Make an authorized v2 request reach the resource server.", status: "candidate",
  max_budget_usdc_micros: "0", acceptance: ["Preserve v1 X-PAYMENT", "Deliver v2 resource with PAYMENT-SIGNATURE"], source_url: "https://github.com/x402-foundation/x402/issues/3837", completed_at_ms: null,
  cost_usdc_micros: "0", fee_funded_usdc_micros: "0", subsidy_usdc_micros: "0", settlement: "unfunded", evidence_status: "unpublished",
  before: null, after: null, patch_url: null, upstream_status: "not_submitted", upstream_url: null, checks: [], transactions: [], attempts: [],
});
const fixture = (): WorkSnapshot => ({ schema_version: 1, generated_at_ms: NOW, mode: "unfunded", pool: null,
  fees: { cvnt_retained_raw: "0", nvdax_collected_raw: "0", nvdax_converted_raw: "0" },
  budget: { available_usdc_micros: "0", reserved_usdc_micros: "0", spent_usdc_micros: "0", subsidy_usdc_micros: "0" }, work: [work()] });
function completed(): Work {
  return { ...work(), status: "completed", completed_at_ms: NOW - 1000, max_budget_usdc_micros: "3000000", cost_usdc_micros: "1305000", fee_funded_usdc_micros: "1305000", settlement: "settled", evidence_status: "verified", before: "Authorized request rejected.", after: "Resource delivered with mocked settlement.", patch_url: "https://github.com/forum-labs/payfetch/commit/abc123", checks: [{ label: "Official middleware interoperability", passed: true }], transactions: (["harvest", "conversion", "deposit", "settlement"] as const).map((kind, i) => ({ kind, signature: String(i + 1).repeat(88) })) };
}
const json = (value: unknown = fixture(), init: ResponseInit = {}) => new Response(JSON.stringify(value), { ...init, headers: { "content-type": "application/json", ...init.headers } });
const render = (data = fixture(), now = NOW, failed = false) => renderToStaticMarkup(<WorkReport data={data} nowMs={now} requestFailed={failed} />);

describe("public work presentation", () => {
  it("uses the shared layout and its own generated social card", () => {
    const html = renderToStaticMarkup(<ComputeWorkPage />);
    expect(html).toContain("Shared site header"); expect(html).toContain("Shared site footer"); expect(html).toContain("Operated with Compute");
    expect(html).toContain("checking the market and recommending how to manage its liquidity");
    expect(html).toContain("Publish the checks, decisions, costs and receipts");
    expect(metadata.alternates?.canonical).toBe("https://opencovenant.org/compute-work");
    expect(metadata.openGraph).toMatchObject({ images: [{ url: "https://opencovenant.org/og/compute-work.png", width: 1200, height: 630 }] });
  });
  it("never fabricates zero balances or a funded state before a response", () => {
    const html = renderToStaticMarkup(<ComputeWorkView />);
    expect(html).toContain("Loading public records"); expect(html).toContain("does not infer funding");
    expect(html).not.toContain("0.00"); expect(html).not.toContain("Live pilot");
  });
  it("distinguishes an observed zero budget, an unfunded pilot, and unknown pool data", () => {
    const html = render(validateSnapshot(fixture()));
    expect(html).toContain("Awaiting funding"); expect(html).toContain("0.00");
    expect(html).toContain("Pool inventory and trading activity have not been observed");
    expect(html).toContain("Candidate · not approved"); expect(html).toContain("Budget not approved"); expect(html).toContain("No contribution is claimed as fee-funded");
  });
  it("ages the snapshot and pool observations independently and marks failed refreshes", () => {
    expect(snapshotCurrent(fixture(), NOW + 180001)).toBe(false);
    expect(snapshotCurrent(fixture(), NOW - 30001)).toBe(false);
    expect(snapshotCurrent(fixture(), NOW, true)).toBe(false);
    expect(render(fixture(), NOW + 180001)).toContain("Current funding state is unknown");
    expect(render(fixture(), NOW, true)).toContain("The latest refresh failed");
    const data = fixture(); data.pool = { address: "1".repeat(32), position_address: "2".repeat(32), active: false, finalized_slot: null, observed_at_ms: NOW - 180001, fee_bps: 65, range: "custom", cvnt_raw: "12345", nvdax_raw: "123", nvdax_multiplier: "1.25", cvnt_fees_owed_raw: "1", nvdax_fees_owed_raw: "0", volume_usdc_micros: null, lp_value_usdc_micros: null, hold_value_usdc_micros: null };
    const html = render(validateSnapshot(data));
    expect(html).toContain("observation stale"); expect(html).toContain("0.65%"); expect(html).toContain("differ from the pilot default");
    expect(html).not.toContain("Current funding state is unknown");
  });
  it("claims fee funding only for completed, verified, settled, fully allocated live work", () => {
    const item = completed(); expect(fullyFeeFunded(item, "live")).toBe(true);
    expect(cardHeadline(item, "live")).toContain("fees paid for");
    for (const mode of ["unfunded", "rehearsal"] as const) expect(fullyFeeFunded(item, mode)).toBe(false);
    const cases: Partial<Work>[] = [{ status: "failed" }, { settlement: "pending" }, { evidence_status: "unchecked" }, { cost_usdc_micros: "0", fee_funded_usdc_micros: "0" }, { fee_funded_usdc_micros: "1000000", subsidy_usdc_micros: "305000" }, { checks: [] }, { checks: [{ label: "Test", passed: false }] }, { patch_url: null }, { transactions: item.transactions.slice(1) }];
    for (const change of cases) expect(fullyFeeFunded({ ...item, ...change }, "live")).toBe(false);
  });
  it("publishes failed attempts and mixed subsidy attribution without a full-fee headline", () => {
    const data = fixture(); data.mode = "live"; data.work = [{ ...completed(), status: "failed", fee_funded_usdc_micros: "1000000", subsidy_usdc_micros: "305000", attempts: [{ id: "attempt-1", status: "failed", cost_usdc_micros: "1305000", summary: "Acceptance failed" }] }];
    const html = render(data);
    expect(html).toContain("1.00 USDC fees + 0.305 USDC subsidy"); expect(html).toContain("Failed attempts remain public");
    expect(html).not.toContain("trading fees paid for");
    const detail = renderToStaticMarkup(<WorkArtifact data={{ schema_version: 1, generated_at_ms: NOW, mode: "live", work: data.work[0] }} />);
    expect(detail).toContain("Acceptance failed"); expect(detail).toContain("Coordinator-reported cost"); expect(detail).toContain("Mocked settlement");
    expect(detail).toContain("Download evidence manifest");
  });
  it("escapes untrusted prose and preserves exact micro-USDC precision", () => {
    const data = fixture(); data.work[0].title = "<script>bad()</script>";
    expect(render(data)).toContain("&lt;script&gt;"); expect(render(data)).not.toContain("<script>");
    expect(usdc("1")).toBe("0.000001 USDC"); expect(usdc("900719925474099312345678" )).toBe("900,719,925,474,099,312.345678 USDC");
    expect(usdc(null)).toBe("Not observed");
  });
});

describe("public work trust boundary", () => {
  it("reconstructs the report and manifest rather than reflecting private extra fields", () => {
    const data = { ...fixture(), buyer: "private-buyer", work: [{ ...work(), credentials: "private-secret", checks: [{ label: "Check", passed: true, logs: "private-logs" }] }], budget: { ...fixture().budget, wallet_key: "private-key" } };
    expect(JSON.stringify(validateSnapshot(data))).not.toContain("private-");
    const m = validateManifest({ schema_version: 1, work_id: work().id, generated_at_ms: NOW, files: [{ name: "task.json", sha256: "a".repeat(64), bytes: 1024, download_url: `https://compute-api.opencovenant.org/community-work/artifacts/${"a".repeat(64)}.json`, signed_auth: "private-secret" }], verification: { work: "unchecked", funding: "unchecked", buyer: "private-buyer" }, limitations: ["Coordinator charges are assertions"], private_jobs: [] }, work().id);
    expect(JSON.stringify(m)).not.toContain("private-");
  });
  it("rejects malformed amounts, excess queue slots, mismatched work IDs and unsafe external links", () => {
    for (const invalid of [-1, 0.1, "-1", "1e6", "001", "1.5", "9".repeat(40)]) {
      expect(() => validateSnapshot({ ...fixture(), budget: { ...fixture().budget, available_usdc_micros: invalid } })).toThrow();
    }
    for (const source_url of ["javascript:alert(1)", "https://github.com.evil.test/a/b/issues/1", "https://user:pass@github.com/a/b/issues/1", "https://github.com/a/b/issues/1?token=secret"]) {
      expect(() => validateSnapshot({ ...fixture(), work: [{ ...work(), source_url }] })).toThrow();
    }
    expect(() => validateSnapshot({ ...fixture(), work: [1, 2, 3, 4].map(i => ({ ...work(), id: `task-${i}`, status: "approved", max_budget_usdc_micros: "1" })) })).toThrow();
    expect(() => validateDetail({ ...fixture(), work: work() }, "another-task")).toThrow();
    expect(() => validateSnapshot({ ...fixture(), work: [{ ...completed(), subsidy_usdc_micros: "1" }] })).toThrow();
  });
  it("does not allow a manifest to point to private routes, another host or oversized artifacts", () => {
    const m = { schema_version: 1, work_id: work().id, generated_at_ms: NOW, files: [{ name: "task.json", sha256: "a".repeat(64), bytes: 10, download_url: `https://compute-api.opencovenant.org/community-work/artifacts/${"a".repeat(64)}.json` }], verification: { work: "unchecked", funding: "unchecked" }, limitations: [] };
    for (const download_url of ["https://evil.test/a.json", "https://compute-api.opencovenant.org/federation/jobs/private/evidence", `${m.files[0].download_url}?token=secret`]) expect(() => validateManifest({ ...m, files: [{ ...m.files[0], download_url }] }, work().id)).toThrow();
    expect(() => validateManifest({ ...m, files: [{ ...m.files[0], bytes: WORK_MAX_BYTES + 1 }] }, work().id)).toThrow();
  });
  it("uses the same-origin credential-free client endpoint and validates errors", async () => {
    const fetcher = vi.fn().mockResolvedValue(json()), signal = new AbortController().signal;
    await expect(fetchSnapshot(signal, fetcher)).resolves.toEqual(fixture());
    expect(fetcher).toHaveBeenCalledWith("/compute-work/data", expect.objectContaining({ credentials: "omit", cache: "no-store", redirect: "error", signal }));
    fetcher.mockResolvedValue(new Response("Private error", { status: 503 }));
    await expect(fetchSnapshot(signal, fetcher)).rejects.toThrow("unavailable");
  });
});

describe("bounded public work proxy", () => {
  it("reads only the fixed public endpoint and strips unknown data", async () => {
    const fetcher = vi.fn().mockResolvedValue(json({ ...fixture(), secret: "do-not-publish" }));
    const response = await proxyWork(new URLSearchParams(), "snapshot", undefined, fetcher);
    expect(response.status).toBe(200); expect(await response.json()).toEqual(fixture());
    expect(response.headers.get("cache-control")).toContain("no-store");
    expect(fetcher).toHaveBeenCalledWith("https://compute-api.opencovenant.org/community-work", expect.objectContaining({ credentials: "omit", redirect: "manual", cache: "no-store", headers: { accept: "application/json" } }));
  });
  it("rejects arbitrary queries and invalid IDs before network access", async () => {
    const fetcher = vi.fn();
    expect((await proxyWork(new URLSearchParams("url=https://evil.test"), "snapshot", undefined, fetcher)).status).toBe(400);
    expect((await proxyWork(new URLSearchParams(), "detail", "../federation/jobs", fetcher)).status).toBe(400);
    expect(fetcher).not.toHaveBeenCalled();
  });
  it.each([
    () => new Response(null, { status: 302, headers: { location: "https://evil.test" } }),
    () => new Response("private-body", { status: 500 }),
    () => json(fixture(), { headers: { "set-cookie": "private=1" } }),
    () => new Response("{}", { headers: { "content-type": "text/html" } }),
    () => json({ schema_version: 2 }),
    () => json(fixture(), { headers: { "content-length": String(WORK_MAX_BYTES + 1) } }),
    () => new Response(" ".repeat(WORK_MAX_BYTES + 1), { headers: { "content-type": "application/json" } }),
  ])("fails closed without exposing rejected upstream bodies", async factory => {
    const response = await proxyWork(new URLSearchParams(), "snapshot", undefined, vi.fn().mockResolvedValue(factory()));
    expect(response.status).toBe(503); expect(await response.json()).toEqual({ error: "Public work data temporarily unavailable" });
  });
  it("checks the detail identifier and uses a safe filename for downloadable manifests", async () => {
    const fetcher = vi.fn().mockResolvedValue(json({ schema_version: 1, generated_at_ms: NOW, mode: "unfunded", work: work() }));
    expect((await proxyWork(new URLSearchParams(), "detail", work().id, fetcher)).status).toBe(200);
    expect((await proxyWork(new URLSearchParams(), "detail", "wrong-id", fetcher)).status).toBe(503);
    fetcher.mockResolvedValue(json({ schema_version: 1, work_id: work().id, generated_at_ms: NOW, files: [], verification: { work: "unchecked", funding: "unchecked" }, limitations: [] }));
    const response = await proxyWork(new URLSearchParams(), "manifest", work().id, fetcher);
    expect(response.headers.get("content-disposition")).toBe('attachment; filename="payfetch-v2-header-manifest.json"');
  });
});
