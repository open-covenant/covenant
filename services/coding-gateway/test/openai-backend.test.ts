import { describe, expect, it } from "vitest";
import type OpenAI from "openai";
import type { ResponseStreamEvent } from "openai/resources/responses/responses.js";
import { OpenaiBackend } from "../src/backends/openai.js";
import { selectBackend } from "../src/backends/index.js";
import type { GatewayEvent, Sandbox } from "../src/types.js";

// Parity is asserted at the GatewayEvent contract (types.ts) — the shared surface
// both backends implement — rather than running the two adapters side by side,
// since the Anthropic adapter constructs its own client and can't be driven
// without a live key. The OpenAI backend accepts an injected client for exactly
// this reason. These tests pin the run / stop / event-mapping behaviour the
// Anthropic adapter also satisfies.

type ResponseLike = {
  id: string;
  status?: string;
  usage?: {
    input_tokens: number;
    output_tokens: number;
    input_tokens_details?: { cached_tokens: number };
  } | null;
  error?: { message?: string; code?: string } | null;
  output: Array<Record<string, unknown>>;
};

type TurnScript = { events: ResponseStreamEvent[] };

function streamOf(events: ResponseStreamEvent[]) {
  return {
    async *[Symbol.asyncIterator]() {
      for (const e of events) yield e;
    },
  };
}

/** Scripted client: each create() call pops the next turn's event stream and
 *  records the (params, options) it was called with. */
function fakeClient(turns: TurnScript[]) {
  const calls: Array<{ params: Record<string, unknown>; options?: { signal?: AbortSignal } }> = [];
  let i = 0;
  const client = {
    responses: {
      create: async (params: Record<string, unknown>, options?: { signal?: AbortSignal }) => {
        calls.push({ params, options });
        const turn = turns[Math.min(i, turns.length - 1)];
        i++;
        return streamOf(turn.events);
      },
    },
  };
  return { client: client as unknown as OpenAI, calls };
}

function textDelta(text: string): ResponseStreamEvent {
  return {
    type: "response.output_text.delta",
    delta: text,
    item_id: "msg_1",
    output_index: 0,
    content_index: 0,
    sequence_number: 1,
    logprobs: [],
  } as ResponseStreamEvent;
}

function reasoningDelta(text: string): ResponseStreamEvent {
  return {
    type: "response.reasoning_summary_text.delta",
    delta: text,
    item_id: "rsn_1",
    output_index: 0,
    summary_index: 0,
    sequence_number: 2,
  } as ResponseStreamEvent;
}

function completed(response: ResponseLike): ResponseStreamEvent {
  return { type: "response.completed", response: response as unknown as never, sequence_number: 99 };
}

function failed(response: ResponseLike): ResponseStreamEvent {
  return { type: "response.failed", response: response as unknown as never, sequence_number: 99 };
}

function messageItem(text: string) {
  return { type: "message", role: "assistant", content: [{ type: "output_text", text }] };
}

function functionCall(callId: string, name: string, args: Record<string, unknown>) {
  return { type: "function_call", call_id: callId, name, arguments: JSON.stringify(args) };
}

/** A recording in-memory sandbox. */
function fakeSandbox(files: Record<string, string> = {}): Sandbox & {
  written: Record<string, string>;
  execs: string[];
} {
  const state = { ...files };
  const written: Record<string, string> = {};
  const execs: string[] = [];
  return {
    readFile: async (p) => state[p] ?? "",
    writeFile: async (p, content) => {
      state[p] = content;
      written[p] = content;
    },
    exec: async (cmd) => {
      execs.push(cmd);
      return { stdout: "ok", stderr: "", exitCode: 0 };
    },
    previewUrl: async () => "https://preview.test",
    destroy: async () => {},
    written,
    execs,
  } as Sandbox & { written: Record<string, string>; execs: string[] };
}

describe("OpenaiBackend.run", () => {
  it("drives a tool-call turn then completes, mapping events and accumulating usage", async () => {
    const turns: TurnScript[] = [
      {
        events: [
          textDelta("Let me write it. "),
          completed({
            id: "resp_1",
            status: "completed",
            usage: { input_tokens: 10, output_tokens: 4, input_tokens_details: { cached_tokens: 2 } },
            output: [functionCall("call_1", "write_file", { path: "a.txt", content: "hi" })],
          }),
        ],
      },
      {
        events: [
          textDelta("Done. "),
          completed({
            id: "resp_2",
            status: "completed",
            usage: { input_tokens: 20, output_tokens: 6, input_tokens_details: { cached_tokens: 0 } },
            output: [messageItem("All done")],
          }),
        ],
      },
    ];
    const { client, calls } = fakeClient(turns);
    const sandbox = fakeSandbox();
    const emitted: GatewayEvent[] = [];

    const backend = new OpenaiBackend(client);
    const result = await backend.run({
      input: "create a.txt",
      sandbox,
      signal: new AbortController().signal,
      emit: (e) => emitted.push(e),
    });

    // Tool executed against the sandbox.
    expect(sandbox.written["a.txt"]).toBe("hi");

    // Event stream matches the contract: text → tool.* / file.written → text → run.completed.
    expect(emitted.map((e) => e.type)).toEqual([
      "message.delta",
      "tool.started",
      "file.written",
      "tool.completed",
      "message.delta",
      "run.completed",
    ]);
    expect(emitted[1]).toMatchObject({ type: "tool.started", tool: "write_file", preview: "a.txt" });
    expect(emitted[2]).toMatchObject({ type: "file.written", path: "a.txt", bytes: 2 });
    expect(emitted[3]).toMatchObject({ type: "tool.completed", tool: "write_file", error: false });
    const done = emitted[5];
    expect(done.type === "run.completed" && done.output).toBe("All done");

    // Usage sums across both turns; OpenAI has no cache-creation tier.
    expect(result.usage).toEqual({
      inputTokens: 30,
      outputTokens: 10,
      cacheReadTokens: 2,
      cacheCreationTokens: 0,
    });

    // Turn 2 threaded on previous_response_id and carried only the tool output.
    expect(calls).toHaveLength(2);
    expect(calls[0]!.params.previous_response_id).toBeUndefined();
    expect(calls[1]!.params.previous_response_id).toBe("resp_1");
    const secondInput = calls[1]!.params.input as Array<{ type: string; call_id: string; output: string }>;
    expect(secondInput).toHaveLength(1);
    expect(secondInput[0]).toMatchObject({ type: "function_call_output", call_id: "call_1" });
    expect(secondInput[0]!.output).toContain("wrote 2 bytes");
  });

  it("maps reasoning-summary deltas to reasoning.available", async () => {
    const { client } = fakeClient([
      {
        events: [
          reasoningDelta("Planning. "),
          completed({ id: "r", status: "completed", usage: null, output: [messageItem("ok")] }),
        ],
      },
    ]);
    const emitted: GatewayEvent[] = [];
    const backend = new OpenaiBackend(client);
    await backend.run({
      input: "x",
      sandbox: fakeSandbox(),
      signal: new AbortController().signal,
      emit: (e) => emitted.push(e),
    });
    expect(emitted[0]).toMatchObject({ type: "reasoning.available", text: "Planning. " });
  });

  it("throws on a pre-aborted signal (POST /stop parity)", async () => {
    const { client, calls } = fakeClient([{ events: [completed({ id: "r", output: [] })] }]);
    const ac = new AbortController();
    ac.abort();
    const backend = new OpenaiBackend(client);
    await expect(
      backend.run({ input: "x", sandbox: fakeSandbox(), signal: ac.signal, emit: () => {} }),
    ).rejects.toThrow("run aborted");
    expect(calls).toHaveLength(0);
  });

  it("forwards the abort signal into the model call so a later stop propagates", async () => {
    const { client, calls } = fakeClient([
      { events: [completed({ id: "r", status: "completed", usage: null, output: [messageItem("ok")] })] },
    ]);
    const ac = new AbortController();
    const backend = new OpenaiBackend(client);
    await backend.run({ input: "x", sandbox: fakeSandbox(), signal: ac.signal, emit: () => {} });
    expect(calls[0]!.options?.signal).toBe(ac.signal);
  });

  it("throws when the model response fails, surfacing the API error", async () => {
    const { client } = fakeClient([
      { events: [failed({ id: "r", error: { code: "rate_limit_exceeded", message: "slow down" } })] },
    ]);
    const backend = new OpenaiBackend(client);
    await expect(
      backend.run({ input: "x", sandbox: fakeSandbox(), signal: new AbortController().signal, emit: () => {} }),
    ).rejects.toThrow("rate_limit_exceeded: slow down");
  });

  it("surfaces a tool error back to the model and marks tool.completed with error", async () => {
    // edit_file against a sandbox where old_string is absent → tool throws.
    const { client, calls } = fakeClient([
      {
        events: [
          completed({
            id: "r1",
            status: "completed",
            usage: null,
            output: [functionCall("c1", "edit_file", { path: "g.txt", old_string: "x", new_string: "y" })],
          }),
        ],
      },
      { events: [completed({ id: "r2", status: "completed", usage: null, output: [messageItem("recovered")] })] },
    ]);
    const emitted: GatewayEvent[] = [];
    const backend = new OpenaiBackend(client);
    await backend.run({ input: "edit", sandbox: fakeSandbox(), signal: new AbortController().signal, emit: (e) => emitted.push(e) });
    expect(emitted.find((e) => e.type === "tool.completed")).toMatchObject({ tool: "edit_file", error: true });
    const secondInput = calls[1]!.params.input as Array<{ output: string }>;
    expect(secondInput[0]!.output).toContain("error:");
  });
});

describe("selectBackend", () => {
  it("defaults to the anthropic backend", () => {
    const b = selectBackend();
    expect(b.id).toBe("anthropic");
  });

  it("selects the openai backend", () => {
    const b = selectBackend("openai");
    expect(b.id).toBe("openai");
    expect(b).toBeInstanceOf(OpenaiBackend);
  });

  it("still selects anthropic explicitly", () => {
    expect(selectBackend("anthropic").id).toBe("anthropic");
  });
});
