import { describe, expect, it } from "vitest";
import type Anthropic from "@anthropic-ai/sdk";
import { withTurnCache } from "../src/backends/anthropic.js";

// The Anthropic backend's run() loop builds its own client with no injection
// seam, so it can't be driven without a live key. withTurnCache is the one
// piece of novel logic — the prompt-cache breakpoint strategy — and it is pure
// over a message array, so it is pinned directly.

type Block = Anthropic.ContentBlockParam;

describe("withTurnCache", () => {
  it("marks only the last block of the last message and leaves the input untouched", () => {
    const messages = [
      { role: "user", content: [{ type: "text", text: "first" }] },
      { role: "assistant", content: [{ type: "text", text: "second" }] },
    ] as Anthropic.MessageParam[];

    const out = withTurnCache(messages);

    // Only the last block of the last message gets the breakpoint.
    expect((out[1]!.content as Block[])[0]).toMatchObject({ cache_control: { type: "ephemeral" } });
    // Earlier messages are untouched.
    expect(((out[0]!.content as Block[])[0] as { cache_control?: unknown }).cache_control).toBeUndefined();
    // The input array is neither replaced nor mutated: cloning the block keeps
    // the breakpoint off the shared reference so it can't accumulate across turns.
    expect(out).not.toBe(messages);
    expect(((messages[1]!.content as Block[])[0] as { cache_control?: unknown }).cache_control).toBeUndefined();
  });

  it("converts string message content into a cached text block", () => {
    const messages = [{ role: "user", content: "hello" }] as Anthropic.MessageParam[];

    const out = withTurnCache(messages);

    expect((out[0]!.content as Block[])[0]).toMatchObject({
      type: "text",
      text: "hello",
      cache_control: { type: "ephemeral" },
    });
  });

  it("passes an empty message list through without throwing", () => {
    expect(withTurnCache([])).toEqual([]);
  });
});
