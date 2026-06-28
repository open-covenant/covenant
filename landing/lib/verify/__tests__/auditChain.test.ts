import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { afterAll, beforeAll, describe, expect, it } from "vitest";
import { recomputeAuditRoot } from "../../audit/auditRoot";
import { checkAnchor2AuditChain } from "../auditChain";

// checkAnchor2AuditChain replays the published canonical event lines through
// recomputeAuditRoot and is green only when the recomputed root matches the
// published audit_root_hex in attestations/<sha>.json. These tests use the real
// recomputeAuditRoot to build valid fixtures and a deliberately wrong root for
// the tamper arm, so the integrity comparison and the malformed-input guards can
// never silently weaken into a false green.
describe("checkAnchor2AuditChain", () => {
  let root: string;
  const STEPS = ["step-a", "step-b", "step-c"];
  const GOOD_ROOT = recomputeAuditRoot(STEPS);
  const WRONG_ROOT = recomputeAuditRoot(["a-different-run"]);
  const dir = () => join(root, "attestations");
  const write = (sha: string, body: unknown) => {
    mkdirSync(dir(), { recursive: true });
    writeFileSync(join(dir(), `${sha}.json`), typeof body === "string" ? body : JSON.stringify(body));
  };
  beforeAll(() => {
    root = mkdtempSync(join(tmpdir(), "cov-chain-"));
  });
  afterAll(() => rmSync(root, { recursive: true, force: true }));

  it("stays yellow when no attestation exists for the commit", () => {
    const w = checkAnchor2AuditChain(root, "absent");
    expect(w.state).toBe("yellow");
    expect(w.detail).toContain("No audit chain published");
  });

  it("turns red when the attestation is missing its root", () => {
    write("noroot", { steps: STEPS });
    const w = checkAnchor2AuditChain(root, "noroot");
    expect(w.state).toBe("red");
    expect(w.detail).toContain("canonical steps");
  });

  it("turns red when the steps array is empty", () => {
    write("nosteps", { audit_root_hex: GOOD_ROOT, steps: [] });
    const w = checkAnchor2AuditChain(root, "nosteps");
    expect(w.state).toBe("red");
    expect(w.detail).toContain("canonical steps");
  });

  it("turns red when a step entry is not a string", () => {
    write("badstep", { audit_root_hex: GOOD_ROOT, steps: ["step-a", 123, "step-c"] });
    const w = checkAnchor2AuditChain(root, "badstep");
    expect(w.state).toBe("red");
    expect(w.detail).toContain("canonical steps");
  });

  it("turns red when the steps field is not an array", () => {
    write("strsteps", { audit_root_hex: GOOD_ROOT, steps: "step-a" });
    const w = checkAnchor2AuditChain(root, "strsteps");
    expect(w.state).toBe("red");
    expect(w.detail).toContain("canonical steps");
  });

  it("turns red when the recomputed root does not match the published root", () => {
    write("tampered", { audit_root_hex: WRONG_ROOT, steps: STEPS });
    const w = checkAnchor2AuditChain(root, "tampered");
    expect(w.state).toBe("red");
    expect(w.detail).toContain("tampered");
  });

  it("turns green when the recomputed root matches the published root", () => {
    write("ok", { audit_root_hex: GOOD_ROOT, steps: STEPS });
    const w = checkAnchor2AuditChain(root, "ok");
    expect(w).toMatchObject({ key: "audit_chain", label: "Audit hash chain", state: "green" });
    expect(w.detail).toContain("recomputed from 3 hash-chained events and matches");
  });

  it("turns red when the attestation JSON is unreadable", () => {
    write("badjson", "{not valid json");
    const w = checkAnchor2AuditChain(root, "badjson");
    expect(w.state).toBe("red");
    expect(w.detail).toContain("unreadable");
  });
});
