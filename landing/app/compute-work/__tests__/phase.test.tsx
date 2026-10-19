import React from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { WorkPilotPhase } from "../_phase";
import ComputeWorkPage from "../page";

const launch = vi.hoisted(() => ({ phase: "awaiting_funding" as WorkPilotPhase, liveView: vi.fn() }));
vi.mock("../_phase", () => ({ workPilotPhase: () => launch.phase }));
vi.mock("../ComputeWork", () => ({ ComputeWorkView: () => { launch.liveView(); return <div>Live reporting component</div>; } }));
vi.mock("../../SiteHeader", () => ({ SiteHeader: () => <header>Shared header</header> }));
vi.mock("../../SiteFooter", () => ({ SiteFooter: () => <footer>Shared footer</footer> }));

afterEach(() => { launch.phase = "awaiting_funding"; launch.liveView.mockClear(); });

describe("public pilot launch phase", () => {
  it("renders the unfunded operator pilot without mounting the fee-funded view or fabricating records", () => {
    const html = renderToStaticMarkup(<ComputeWorkPage />);
    expect(launch.liveView).not.toHaveBeenCalled();
    expect(html).toContain("Unfunded pilot");
    expect(html).toContain("The pool is not funded.");
    expect(html).toContain("Operated with Compute.");
    expect(html).toContain("preparing a small CVNT/NVDAx pool on Orca");
    expect(html).toContain("The owner approves every transaction.");
    expect(html).toContain("Loading the latest public checks");
    expect(html).toContain("$850");
    expect(html).not.toContain("0.00");
    expect(html).not.toContain("/compute-work/data");
    expect(html).not.toContain("Awaiting liquidity");
    expect(html).not.toContain("Live reporting component");
    expect(html).not.toContain("fees paid for this contribution");
  });

  it("retains the live data view behind an explicit release phase change", () => {
    launch.phase = "live";
    const html = renderToStaticMarkup(<ComputeWorkPage />);
    expect(launch.liveView).toHaveBeenCalledOnce();
    expect(html).toContain("Live reporting component");
    expect(html).toContain("Our CVNT/NVDAx pool on Orca");
    expect(html).toContain("Preparation evidence");
    expect(html).toContain("do not establish the pool’s current funding or performance");
    expect(html).not.toContain("Unfunded pilot");
    expect(html).not.toContain("The pool is not funded.");
  });
});
