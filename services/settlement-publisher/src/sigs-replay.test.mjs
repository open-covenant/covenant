import { describe, it, expect } from "vitest";
import { restoreSigs } from "./sigs-replay.mjs";

const row = (intent_id, tx_sig, extra = {}) =>
  JSON.stringify({ intent_id, tx_sig, ...extra });

describe("restoreSigs", () => {
  it("restores a well-formed row keyed by intent_id with all fields", () => {
    const text = row("i1", "s1", { slot: 100, cluster: "devnet" });
    const m = restoreSigs(text);
    expect(m.size).toBe(1);
    expect(m.get("i1")).toEqual({ intent_id: "i1", tx_sig: "s1", slot: 100, cluster: "devnet" });
  });

  it("drops a row missing tx_sig", () => {
    const m = restoreSigs(JSON.stringify({ intent_id: "i2" }));
    expect(m.has("i2")).toBe(false);
    expect(m.size).toBe(0);
  });

  it("drops a row missing intent_id", () => {
    const m = restoreSigs(JSON.stringify({ tx_sig: "s3" }));
    expect(m.size).toBe(0);
  });

  it("skips a malformed JSON line without throwing", () => {
    expect(() => restoreSigs("{not json")).not.toThrow();
    expect(restoreSigs("{not json").size).toBe(0);
  });

  it("skips a JSON null line without throwing", () => {
    expect(() => restoreSigs("null")).not.toThrow();
    expect(restoreSigs("null").size).toBe(0);
  });

  it("skips blank and whitespace-only lines", () => {
    const text = `\n  \n${row("i1", "s1")}\n\t\n`;
    const m = restoreSigs(text);
    expect(m.size).toBe(1);
    expect(m.has("i1")).toBe(true);
  });

  it("keeps the last row for a duplicate intent_id", () => {
    const text = `${row("i1", "first")}\n${row("i1", "second")}`;
    const m = restoreSigs(text);
    expect(m.size).toBe(1);
    expect(m.get("i1").tx_sig).toBe("second");
  });

  it("restores multiple distinct rows", () => {
    const text = `${row("i1", "s1")}\n${row("i2", "s2")}\n${row("i3", "s3")}`;
    const m = restoreSigs(text);
    expect(m.size).toBe(3);
    expect([...m.keys()].sort()).toEqual(["i1", "i2", "i3"]);
  });
});
