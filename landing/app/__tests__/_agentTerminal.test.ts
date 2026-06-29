import { describe, expect, it } from "vitest";
import { type AgentEvent, formatEvent, stateKind } from "../_agentTerminal";

// stateKind and formatEvent turn the streamed autonomy events into the colored
// lines painted by the live terminal. stateKind decides a transition's hue from
// its target state; formatEvent renders a commit or a transition into the exact
// Line[] the feed appends. These pin every state-to-kind arm and every
// formatEvent branch — the optional stat/note lines, the blank separators, the
// indentation, and the unknown-event fallthrough — so the public feed can't
// silently mis-color or mangle an event.

describe("stateKind", () => {
  it("maps each terminal-coloring state", () => {
    expect(stateKind("integrated")).toBe("add");
    expect(stateKind("ready")).toBe("write");
    expect(stateKind("blocked")).toBe("del");
    expect(stateKind("repair")).toBe("del");
    expect(stateKind("validation")).toBe("hunk");
    expect(stateKind("cross_review")).toBe("hunk");
    expect(stateKind("self_review")).toBe("hunk");
  });

  it("falls back to meta for the in-flight states", () => {
    for (const s of ["proposed", "triaged", "planned", "in_progress", ""]) {
      expect(stateKind(s)).toBe("meta");
    }
  });
});

describe("formatEvent", () => {
  it("renders a commit with a stat line", () => {
    const e: AgentEvent = { type: "commit", hash: "abc123", subject: "do x", stat: "2 files" };
    expect(formatEvent(e)).toEqual([
      { k: "commit", t: "commited abc123  # do x" },
      { k: "meta", t: "  2 files" },
      { k: "blank", t: "" },
    ]);
  });

  it("omits the stat line when the commit has none", () => {
    const e: AgentEvent = { type: "commit", hash: "abc123", subject: "do x", stat: "" };
    expect(formatEvent(e)).toEqual([
      { k: "commit", t: "commited abc123  # do x" },
      { k: "blank", t: "" },
    ]);
  });

  it("renders a transition, coloring the head line by target state", () => {
    const e: AgentEvent = {
      type: "transition",
      taskId: "task-1",
      from: "validation",
      to: "integrated",
      actor: "implementer",
      note: "shipped",
    };
    expect(formatEvent(e)).toEqual([
      { k: "add", t: "[integrated] task-1" },
      { k: "ctx", t: "    shipped" },
      { k: "blank", t: "" },
    ]);
  });

  it("omits the note line when the transition has none", () => {
    const e: AgentEvent = {
      type: "transition",
      taskId: "task-1",
      from: "planned",
      to: "in_progress",
      actor: "implementer",
      note: "",
    };
    expect(formatEvent(e)).toEqual([
      { k: "meta", t: "[in_progress] task-1" },
      { k: "blank", t: "" },
    ]);
  });

  it("yields nothing for an unrecognized event", () => {
    expect(formatEvent({ type: "boot" } as unknown as AgentEvent)).toEqual([]);
  });
});
