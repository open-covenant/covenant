import React from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it, vi } from "vitest";
import ComputeRentalChecksPage, { metadata } from "../page";
import { RentalChecksReport, RentalChecksView } from "../RentalChecks";
import { fetchRentalChecks, heartbeatCurrent, recentRuns, RENTAL_CHECKS_MAX_BYTES, validateRentalChecks, type RentalChecks } from "../_status";
import { proxyRentalChecks } from "../_proxy";
import { GET } from "../data/route";
import { rentalChecksRelease } from "../_release";
vi.mock("../../SiteHeader",()=>({SiteHeader:()=> <header>Shared site header</header>}));
vi.mock("../../SiteFooter",()=>({SiteFooter:()=> <footer>Shared site footer</footer>}));
const NOW=1802908860;
const fixture=():RentalChecks=>({
  schema:1,generated_at_unix:NOW-60,server_time_unix:NOW,stale_after_seconds:180,observed_state_age_seconds:60,is_stale:false,feed_status:"fresh",enabled:true,cadence_seconds:21600,next_due_at_unix:NOW+21600,pause_reason:null,
  budget:{daily_limit_micro_usd:2000000,pilot_limit_micro_usd:20000000,daily_reserved_micro_usd:500000,pilot_reserved_micro_usd:1500000},
  monitor:{consecutive_unsuccessful_windows:0,alert_due:false,last_success_at_unix:NOW-360},
  latest:{run_id:"00000000-0000-4000-8000-000000000001",job_id:"00000000-0000-4000-8000-000000000002",started_at_unix:NOW-660,finished_at_unix:NOW-360,verdict:"success",stage:"provider_cleanup",reason:null,gpu_model:"NVIDIA L40S",charged_micro_usdc:10000,refunded_micro_usdc:90000,ssh_identity_verified:true,gpu_probe_verified:true,provider_cleanup_verified:true,balance_reconciled:true,settlement_verified_onchain:true,recovery_only:false},history:[],
});
const render=(data=fixture(),now=NOW*1000,failed=false)=>renderToStaticMarkup(<RentalChecksReport data={data} nowMs={now} requestFailed={failed}/>);
const response=(data:unknown=fixture(),headers:Record<string,string>={})=>new Response(JSON.stringify(data),{headers:{"content-type":"application/json","cache-control":"no-store",...headers}});

describe("rental results and reporting freshness",()=>{
  it("publishes the accepted page with shared layout, real-data loading and generated card",async()=>{
    const html=renderToStaticMarkup(<ComputeRentalChecksPage/>);
    expect(html).toContain("Shared site header");expect(html).toContain("Shared site footer");
    expect(html).toContain("Loading rental checks");expect(html).not.toContain("NVIDIA L40S");
    expect(html).not.toContain("Release preview");
    expect(metadata.robots).toBeUndefined();
    expect(metadata.alternates?.canonical).toBe("https://opencovenant.org/compute-rental-checks");
    expect(metadata.twitter).toMatchObject({card:"summary_large_image"});
    expect(metadata.openGraph).toMatchObject({images:[{url:"https://opencovenant.org/og/compute-rental-checks.png",width:1200,height:630}]});
    const fetcher=vi.fn().mockResolvedValue(response());vi.stubGlobal("fetch",fetcher);
    try { expect((await GET(new Request("https://opencovenant.org/compute-rental-checks/data"))).status).toBe(200); }
    finally { vi.unstubAllGlobals(); }
  });
  it("closes the data route and hides reports when admissions are not released",async()=>{
    const previous=rentalChecksRelease.accepted;rentalChecksRelease.accepted=false;
    try {
      const html=renderToStaticMarkup(<ComputeRentalChecksPage/>);
      expect(html).toContain("Awaiting production acceptance");
      expect((await GET(new Request("https://opencovenant.org/compute-rental-checks/data"))).status).toBe(404);
    } finally { rentalChecksRelease.accepted=previous; }
  });
  it("does not invent a pass or counts before the first response",()=>{
    const html=renderToStaticMarkup(<RentalChecksView/>);expect(html).toContain("Loading rental checks");expect(html).not.toContain("Passed");expect(html).not.toContain("0 USD");
  });
  it("ages the heartbeat separately from the six-hour rental result",()=>{
    const data=fixture();data.latest!.started_at_unix-=20000;data.latest!.finished_at_unix!-=20000;
    expect(heartbeatCurrent(data,NOW*1000)).toBe(true);expect(render(data)).toContain("Current report");
    expect(heartbeatCurrent(data,(NOW+121)*1000)).toBe(false);expect(heartbeatCurrent(data,(NOW-31)*1000)).toBe(false);
    const old=render(data,(NOW+121)*1000);expect(old).toContain("Current state unknown");expect(old).toContain("Passed · last report");expect(old).not.toContain("text-ok");
    expect(render(data,NOW*1000,true)).toContain("latest refresh failed");expect(heartbeatCurrent(data,NOW*1000,true)).toBe(false);
  });
  it.each(["stale","invalid","not_observed","disabled"] as const)("does not advertise a live pass for a %s feed",status=>{
    const data=fixture();data.feed_status=status;data.is_stale=true;expect(heartbeatCurrent(data,NOW*1000)).toBe(false);expect(render(data)).not.toContain("text-ok");
  });
  it("displays an intentional pilot pause separately from the rental verdict",()=>{
    const data=fixture();data.enabled=false;data.feed_status="disabled";data.pause_reason="pilot_budget_exhausted";data.next_due_at_unix=null;
    const html=render(data);expect(html).toContain("New rentals paused");expect(html).toContain("Current report");expect(html).toContain("Pilot budget exhausted");expect(html).toContain("Passed");expect(html).not.toContain("Failed");
  });
  it("does not let a disabled snapshot conceal an overdue heartbeat",()=>{
    const data=fixture();Object.assign(data,{enabled:false,feed_status:"disabled",pause_reason:"pilot_budget_exhausted",is_stale:true,generated_at_unix:NOW-181,observed_state_age_seconds:181});
    const html=render(data);expect(html).toContain("Current state unknown");expect(html).toContain("reporting heartbeat is overdue");expect(html).not.toContain("Current report");expect(html).toContain("New rentals paused · last report");
  });
  it("keeps recovery distinct from a successful GPU test",()=>{
    const data=fixture();Object.assign(data.latest!,{verdict:"recovered",recovery_only:true,gpu_probe_verified:false,ssh_identity_verified:false,reason:"prior_rental_reconciled"});
    const html=render(validateRentalChecks(data));expect(html).toContain("Recovery complete");expect(html).toContain("not a new passed rental");expect(html).not.toContain("text-ok");
  });
  it("preserves unknown values in a missing-state skeleton",()=>{
    const data:RentalChecks={...fixture(),generated_at_unix:null,observed_state_age_seconds:null,is_stale:true,feed_status:"not_observed",enabled:false,next_due_at_unix:null,latest:null,history:[],budget:null,monitor:null};
    const html=render(validateRentalChecks(data));expect(html).toContain("Awaiting first report");expect(html).toContain("No rental result has been recorded");expect(html).toContain("no zero balance is inferred");expect(html).not.toContain("0 USD");
  });
  it("keeps unfinished verification unknown instead of failed",()=>{
    const data=fixture();Object.assign(data.latest!,{verdict:"pending",finished_at_unix:null,provider_cleanup_verified:false,settlement_verified_onchain:false,balance_reconciled:false,stage:"settlement",charged_micro_usdc:null,refunded_micro_usdc:null});
    const html=render(validateRentalChecks(data));expect(html).toContain("In progress");expect(html).toContain("Not verified");expect(html).toContain("Not confirmed");expect(html).not.toContain("Failed");
  });
  it("deduplicates the latest history row",()=>{const data=fixture();data.history=[structuredClone(data.latest!)];expect(recentRuns(validateRentalChecks(data))).toHaveLength(1);});
});

describe("rental-check public contract",()=>{
  it.each(["ssh_identity_verified","gpu_probe_verified","provider_cleanup_verified","settlement_verified_onchain","balance_reconciled"] as const)("rejects a success missing %s",field=>{
    const data=fixture();data.latest![field]=false;expect(()=>validateRentalChecks(data)).toThrow("invalid");
  });
  it("accepts the final server stage vocabulary",()=>{
    for(const stage of ["budget","complete","recovery","monitor"]){const data=fixture();data.latest!.stage=stage;expect(validateRentalChecks(data).latest!.stage).toBe(stage);}
  });
  it("rejects conflicting history, unknown stages, unsafe reasons and malformed numbers",()=>{
    const conflict=fixture();conflict.history=[{...conflict.latest!,reason:"different"}];
    const cases=[conflict,{...fixture(),schema:2},{...fixture(),is_stale:true},{...fixture(),cadence_seconds:60},{...fixture(),history:Array(41).fill(fixture().latest)},{...fixture(),budget:{...fixture().budget,daily_reserved_micro_usd:-1}},{...fixture(),latest:{...fixture().latest,stage:"unknown_stage"}},{...fixture(),latest:{...fixture().latest,reason:"https://private/token"}},{...fixture(),latest:{...fixture().latest,job_id:"not-a-uuid"}}];
    for(const data of cases)expect(()=>validateRentalChecks(data)).toThrow("invalid");
  });
  it("drops extra private fields at every reconstructed level",()=>{
    const data={...fixture(),provider_token:"secret",budget:{...fixture().budget,wallet_key:"secret"},latest:{...fixture().latest,ssh_endpoint:"private",signed_storage_url:"private"}};
    expect(JSON.stringify(validateRentalChecks(data))).not.toMatch(/secret|private|ssh_endpoint|signed_storage_url/);
  });
  it("uses a bounded same-origin client fetch without credentials",async()=>{
    const fetcher=vi.fn().mockResolvedValue(response());const abort=new AbortController();await expect(fetchRentalChecks(abort.signal,fetcher)).resolves.toEqual(fixture());
    expect(fetcher).toHaveBeenCalledWith("/compute-rental-checks/data",expect.objectContaining({credentials:"omit",cache:"no-store",redirect:"error",signal:abort.signal}));
    fetcher.mockResolvedValue(new Response(" ".repeat(RENTAL_CHECKS_MAX_BYTES+1),{headers:{"content-type":"application/json"}}));await expect(fetchRentalChecks(abort.signal,fetcher)).rejects.toThrow("invalid");
  });
});

describe("fixed-origin rental-check proxy",()=>{
  it("returns only valid no-store report data",async()=>{
    const fetcher=vi.fn().mockResolvedValue(response());const result=await proxyRentalChecks(new URLSearchParams(),fetcher);
    expect(result.status).toBe(200);expect(result.headers.get("cache-control")).toContain("no-store");expect(await result.json()).toEqual(fixture());
    expect(fetcher).toHaveBeenCalledWith("https://compute-api.opencovenant.org/rental-checks",expect.objectContaining({redirect:"manual",credentials:"omit",cache:"no-store"}));
  });
  it("rejects proxy queries before contacting upstream",async()=>{const fetcher=vi.fn();expect((await proxyRentalChecks(new URLSearchParams("url=https://example.test"),fetcher)).status).toBe(400);expect(fetcher).not.toHaveBeenCalled();});
  it.each([
    ()=>new Response(null,{status:302,headers:{location:"https://example.test/private"}}),()=>new Response("private error",{status:500}),
    ()=>response(fixture(),{"set-cookie":"private=secret"}),()=>response(fixture(),{"content-type":"text/html"}),()=>response({schema:99}),
    ()=>response(fixture(),{"content-length":String(RENTAL_CHECKS_MAX_BYTES+1)}),()=>new Response(" ".repeat(RENTAL_CHECKS_MAX_BYTES+1),{headers:{"content-type":"application/json"}}),
  ])("blocks redirected, private, malformed or oversized responses",async factory=>{
    const result=await proxyRentalChecks(new URLSearchParams(),vi.fn().mockResolvedValue(factory()));expect(result.status).toBe(503);expect(await result.json()).toEqual({error:"Rental-check data temporarily unavailable"});
  });
});
