import React from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { WorkPilotPhase } from "../_phase";
import ComputeWorkPage from "../page";

const launch = vi.hoisted(() => ({ phase: "awaiting_funding" as WorkPilotPhase, liveView: vi.fn() }));
vi.mock("../_phase", () => ({ get WORK_PILOT_PHASE() { return launch.phase; } }));
vi.mock("../ComputeWork", () => ({ ComputeWorkView: () => { launch.liveView(); return <div>Live reporting component</div>; } }));
vi.mock("../../SiteHeader", () => ({ SiteHeader: () => <header>Shared header</header> }));
vi.mock("../../SiteFooter", () => ({ SiteFooter: () => <footer>Shared footer</footer> }));

afterEach(() => { launch.phase = "awaiting_funding"; launch.liveView.mockClear(); });

describe("public pilot launch phase", () => {
  it("renders the waiting launch without mounting the live snapshot poller or fabricating records", () => {
    const html = renderToStaticMarkup(<ComputeWorkPage />);
    expect(launch.liveView).not.toHaveBeenCalled();
    expect(html).toContain("Awaiting funding");
    expect(html).toContain("The pool is not funded yet");
    expect(html).toContain("uses existing CVNT and NVDAx");
    expect(html).toContain("preparing a CVNT/NVDAx pool on Orca");
    expect(html).toContain("tasks we approve in advance, each with a fixed budget");
    expect(html).not.toContain("0.00");
    expect(html).not.toContain("/compute-work/data");
    expect(html).not.toContain("Refresh");
    expect(html).not.toContain("Report generated");
    expect(html).not.toContain("Live reporting component");
    expect(html).not.toContain("fees paid for this contribution");
  });

  it("retains the live data view behind an explicit release phase change", () => {
    launch.phase = "live";
    const html = renderToStaticMarkup(<ComputeWorkPage />);
    expect(launch.liveView).toHaveBeenCalledOnce();
    expect(html).toContain("Live reporting component");
    expect(html).toContain("Our CVNT/NVDAx pool on Orca");
    expect(html).not.toContain("Awaiting funding");
    expect(html).not.toContain("The pool is not funded yet");
  });
});
