import { afterEach, describe, expect, it, vi } from "vitest";
import { workPilotPhase } from "../_phase";

afterEach(() => vi.unstubAllEnvs());
describe("operator-controlled server release setting", () => {
  it("requires the exact live setting and otherwise stays unfunded", () => {
    for (const value of ["", "LIVE", "true", "false", " live", "awaiting_funding"]) expect(workPilotPhase(value)).toBe("awaiting_funding");
    expect(workPilotPhase("live")).toBe("live");
    vi.stubEnv("COVENANT_COMPUTE_WORK_PHASE", undefined);
    expect(workPilotPhase()).toBe("awaiting_funding");
  });
  it("reads the environment for each request instead of fixing it at module load", () => {
    vi.stubEnv("COVENANT_COMPUTE_WORK_PHASE", "live");
    expect(workPilotPhase()).toBe("live");
    vi.stubEnv("COVENANT_COMPUTE_WORK_PHASE", "awaiting_funding");
    expect(workPilotPhase()).toBe("awaiting_funding");
  });
});
