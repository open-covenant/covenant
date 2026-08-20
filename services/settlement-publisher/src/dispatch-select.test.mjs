import { describe, it, expect } from "vitest";
import { selectDispatches } from "./dispatch-select.mjs";

const dispatched = (id, timestamp_ms) => ({
  id,
  timestamp_ms,
  kind: { type: "intent_dispatched", intent_id: id },
});
const other = (type, id, timestamp_ms) => ({
  id,
  timestamp_ms,
  kind: { type },
});

describe("selectDispatches", () => {
  it("keeps only intent_dispatched events", () => {
    const events = [
      other("memory_written", "m1", 1),
      dispatched("d1", 2),
      other("skill_tx_signed", "s1", 3),
      dispatched("d2", 4),
    ];
    expect(selectDispatches(events).map((e) => e.id)).toEqual(["d1", "d2"]);
  });

  it("orders oldest -> newest by timestamp_ms", () => {
    const events = [dispatched("a", 30), dispatched("b", 10), dispatched("c", 20)];
    expect(selectDispatches(events).map((e) => e.id)).toEqual(["b", "c", "a"]);
  });

  it("treats a missing timestamp_ms as 0 (sorts first)", () => {
    const events = [dispatched("has_ts", 5), dispatched("no_ts", undefined)];
    expect(selectDispatches(events).map((e) => e.id)).toEqual(["no_ts", "has_ts"]);
  });

  it("returns an empty array for empty input", () => {
    expect(selectDispatches([])).toEqual([]);
  });

  it("does not mutate the input order semantics (filter is order-preserving before sort)", () => {
    const events = [dispatched("a", 1), dispatched("b", 1)];
    const out = selectDispatches(events).map((e) => e.id);
    expect(out).toEqual(["a", "b"]);
  });

  it("drops malformed feed entries (null, undefined, missing kind) without throwing", () => {
    const events = [null, undefined, { id: "x" }, dispatched("d1", 1)];
    expect(() => selectDispatches(events)).not.toThrow();
    expect(selectDispatches(events).map((e) => e.id)).toEqual(["d1"]);
  });
});
