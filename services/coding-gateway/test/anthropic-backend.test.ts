import { describe, expect, it } from "vitest";
import type Anthropic from "@anthropic-ai/sdk";
import { execTool, previewOf, withTurnCache } from "../src/backends/anthropic.js";
import type { Sandbox } from "../src/types.js";

// The Anthropic backend's run() loop builds its own client with no injection
// seam, so it can't be driven without a live key. The novel logic lives in
// pure / injectable helpers: withTurnCache (prompt-cache breakpoint strategy,
// pure over a message array) and execTool (tool dispatch, takes an injected
// sandbox so it is mock-drivable with no key). Both are pinned directly.

function memSandbox(files: Record<string, string> = {}): Sandbox {
  const store = { ...files };
  return {
    readFile: async (p) => {
      if (!(p in store)) throw new Error(`ENOENT: ${p}`);
      return store[p]!;
    },
    writeFile: async (p, c) => {
      store[p] = c;
    },
    exec: async () => ({ stdout: "", stderr: "", exitCode: 0 }),
    previewUrl: async () => "",
    destroy: async () => {},
  };
}

function toolUse(name: string, input: Record<string, unknown>): Anthropic.ToolUseBlock {
  return { id: "toolu_1", name, input } as Anthropic.ToolUseBlock;
}

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

describe("execTool", () => {
  it("throws when edit_file old_string is not found", async () => {
    const sandbox = memSandbox({ "a.txt": "hello world" });
    const call = execTool(
      toolUse("edit_file", { path: "a.txt", old_string: "missing", new_string: "x" }),
      sandbox,
      () => {},
    );
    await expect(call).rejects.toThrow(/not found in a\.txt/);
  });

  it("throws when edit_file old_string is not unique", async () => {
    const sandbox = memSandbox({ "a.txt": "x x" });
    const call = execTool(
      toolUse("edit_file", { path: "a.txt", old_string: "x", new_string: "y" }),
      sandbox,
      () => {},
    );
    await expect(call).rejects.toThrow(/not unique in a\.txt/);
  });

  it("throws for an unknown tool", async () => {
    const call = execTool(toolUse("delete_file", { path: "a.txt" }), memSandbox(), () => {});
    await expect(call).rejects.toThrow(/unknown tool: delete_file/);
  });
});

describe("previewOf", () => {
  it("truncates a bash command to 120 chars for the event preview", () => {
    const preview = previewOf(toolUse("bash", { command: "x".repeat(200) }));
    expect(preview).toHaveLength(120);
    expect(preview).toBe("x".repeat(120));
  });

  it("returns the path for a path-bearing tool", () => {
    expect(previewOf(toolUse("read_file", { path: "src/app.ts" }))).toBe("src/app.ts");
  });

  it("returns empty for a tool with neither a command nor a path", () => {
    expect(previewOf(toolUse("custom", {}))).toBe("");
  });
});
