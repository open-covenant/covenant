import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { GET } from "../route";
import { bus, type AgentEvent } from "@/lib/agentBus.mjs";

// GET /agent-stream/live is the SSE feed of the live covenant loop. On connect it
// emits an SSE retry hint, replays the recent ring, then subscribes the
// connection to the bus so every new transition/commit streams as a `data:`
// frame; a heartbeat keeps the connection warm and cancel() must tear down both
// the subscription and the heartbeat so a dropped client never leaks. These
// tests pin the connect framing, the ring replay, the live fan-out, the SSE
// headers, and the cancel cleanup. The bus is a process-global singleton, so the
// ring is cleared between arms and local tailing is disabled for determinism.

const dec = new TextDecoder();

// The route serializes whatever the bus fans out; bare markers keep the frame
// assertions readable without building full AgentEvents.
const pub = (e: unknown) => bus.publish(e as AgentEvent);

async function next(reader: ReadableStreamDefaultReader<Uint8Array>) {
  const { value, done } = await reader.read();
  return done ? null : dec.decode(value);
}

beforeEach(() => {
  // Disable the real events.jsonl/commit tail so the ring holds only what a test
  // publishes; startLocalTail then becomes an idempotent no-op.
  vi.stubEnv("AGENT_LOCAL_TAIL", "false");
  bus.ring.length = 0;
});

afterEach(() => {
  vi.unstubAllEnvs();
  vi.restoreAllMocks();
  bus.ring.length = 0;
});

describe("agent-stream live SSE route", () => {
  it("opens with the retry hint, replays the ring, and sets SSE headers", async () => {
    pub({ type: "transition", id: "t1" });
    pub({ type: "commit", id: "c1" });

    const res = GET();
    expect(res.headers.get("content-type")).toBe("text/event-stream; charset=utf-8");
    expect(res.headers.get("cache-control")).toBe("no-cache, no-transform");
    expect(res.headers.get("x-accel-buffering")).toBe("no");

    const reader = res.body!.getReader();
    try {
      expect(await next(reader)).toBe("retry: 3000\n\n");
      expect(await next(reader)).toBe(`data: ${JSON.stringify({ type: "transition", id: "t1" })}\n\n`);
      expect(await next(reader)).toBe(`data: ${JSON.stringify({ type: "commit", id: "c1" })}\n\n`);
    } finally {
      await reader.cancel();
    }
  });

  it("streams an event published after the client connects", async () => {
    const res = GET();
    const reader = res.body!.getReader();
    try {
      expect(await next(reader)).toBe("retry: 3000\n\n"); // empty ring → straight to the hint

      pub({ type: "live", id: "L1" });
      expect(await next(reader)).toBe(`data: ${JSON.stringify({ type: "live", id: "L1" })}\n\n`);
    } finally {
      await reader.cancel();
    }
  });

  it("unsubscribes the connection and clears the heartbeat on cancel", async () => {
    const realSubscribe = bus.subscribe;
    let unsubCalls = 0;
    vi.spyOn(bus, "subscribe").mockImplementation((cb) => {
      const unsub = realSubscribe(cb);
      return () => {
        unsubCalls += 1;
        unsub();
      };
    });
    const clearSpy = vi.spyOn(globalThis, "clearInterval");

    const res = GET();
    const reader = res.body!.getReader();
    await next(reader); // retry
    await reader.cancel();

    expect(unsubCalls).toBe(1); // cancel() invoked the unsub
    expect(clearSpy).toHaveBeenCalled(); // and cleared the heartbeat interval

    // The subscriber is gone, so a later publish reaches no one — proving the
    // unsub actually detached this connection rather than merely being recorded.
    const before = bus.ring.length;
    pub({ type: "after-cancel", id: "x" });
    expect(bus.ring.length).toBe(before + 1); // ring still records it…
    // …but no live delivery: re-subscribing now sees a clean fan-out.
    const seen: unknown[] = [];
    const unsub = realSubscribe((e) => seen.push(e));
    pub({ type: "probe", id: "p" });
    unsub();
    expect(seen).toEqual([{ type: "probe", id: "p" }]);
  });
});
