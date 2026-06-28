import { describe, expect, it } from "vitest";
import { clean, parseTransitionLine } from "../agentBus.mjs";

const line = (o: Record<string, unknown>) => JSON.stringify(o);

// parseTransitionLine feeds the public live agent feed: it must fail closed on a
// malformed or incomplete transition line and redaction-clean the taskId/note it
// forwards. Identity-token redaction itself is covered in agentStream.test.ts;
// here we pin the parser arms and the field wiring with token-free vectors.
describe("parseTransitionLine", () => {
  it("returns null for malformed JSON", () => {
    expect(parseTransitionLine("{not json")).toBeNull();
  });

  it("returns null for a non-object payload", () => {
    expect(parseTransitionLine("null")).toBeNull();
  });

  it("returns null when taskId is missing", () => {
    expect(parseTransitionLine(line({ to: "ready" }))).toBeNull();
  });

  it("returns null when to is missing", () => {
    expect(parseTransitionLine(line({ taskId: "demo-task" }))).toBeNull();
  });

  it("normalizes a full transition line", () => {
    const out = parseTransitionLine(
      line({
        timestamp: "2026-06-28T00:00:00Z",
        taskId: "demo-task",
        from: "planned",
        to: "in_progress",
        actorRole: "implementer",
        note: "a transition note",
      }),
    );
    expect(out).toEqual({
      type: "transition",
      ts: "2026-06-28T00:00:00Z",
      taskId: "demo-task",
      from: "planned",
      to: "in_progress",
      actor: "implementer",
      note: "a transition note",
    });
  });

  it("defaults optional fields to empty strings when only taskId and to are present", () => {
    expect(parseTransitionLine(line({ taskId: "demo-task", to: "ready" }))).toEqual({
      type: "transition",
      ts: "",
      taskId: "demo-task",
      from: "",
      to: "ready",
      actor: "",
      note: "",
    });
  });
});

describe("clean", () => {
  it("maps a dropped (nullish) field to an empty string, never null", () => {
    expect(clean(null)).toBe("");
  });

  it("passes a clean field through unchanged", () => {
    expect(clean("a normal field")).toBe("a normal field");
  });
});
