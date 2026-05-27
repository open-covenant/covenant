import Anthropic from "@anthropic-ai/sdk";
import { config } from "../config.js";
import type { CodingBackend, GatewayEvent, Sandbox, TokenUsage } from "../types.js";

const MAX_TOKENS = 64_000;
const MAX_TURNS = 60;

const SYSTEM = `You are a coding agent working inside a sandboxed workspace.
Build what the user asks: create and edit files, run commands, install
dependencies, and verify your work by running it. The working directory is the
workspace root; use relative paths. Prefer small, verifiable steps — write a
file, run it, read the output, fix. When the task is a web app, leave it
runnable (e.g. a dev server on a known port) and say which port. Finish with a
short summary of what you built and how to run it.`;

const TOOLS: Anthropic.Tool[] = [
  {
    name: "read_file",
    description: "Read a UTF-8 file from the workspace.",
    input_schema: {
      type: "object",
      properties: { path: { type: "string", description: "Path relative to the workspace root" } },
      required: ["path"],
    },
  },
  {
    name: "write_file",
    description: "Create or overwrite a UTF-8 file in the workspace.",
    input_schema: {
      type: "object",
      properties: {
        path: { type: "string", description: "Path relative to the workspace root" },
        content: { type: "string", description: "Full file contents" },
      },
      required: ["path", "content"],
    },
  },
  {
    name: "edit_file",
    description:
      "Replace an exact, unique substring in an existing file. Fails if old_string is missing or appears more than once.",
    input_schema: {
      type: "object",
      properties: {
        path: { type: "string" },
        old_string: { type: "string", description: "Exact text to replace" },
        new_string: { type: "string", description: "Replacement text" },
      },
      required: ["path", "old_string", "new_string"],
    },
  },
  {
    name: "bash",
    description: "Run a shell command in the workspace and return stdout, stderr, and exit code.",
    input_schema: {
      type: "object",
      properties: { command: { type: "string" } },
      required: ["command"],
    },
  },
];

export class AnthropicBackend implements CodingBackend {
  readonly id = "anthropic" as const;
  private readonly client: Anthropic;

  constructor(apiKey?: string) {
    this.client = apiKey ? new Anthropic({ apiKey }) : new Anthropic();
  }

  async run(opts: {
    input: string;
    sandbox: Sandbox;
    signal: AbortSignal;
    emit: (e: GatewayEvent) => void;
  }): Promise<{ output: string; usage: TokenUsage }> {
    const { input, sandbox, signal, emit } = opts;
    const messages: Anthropic.MessageParam[] = [{ role: "user", content: input }];
    const usage: TokenUsage = {
      inputTokens: 0,
      outputTokens: 0,
      cacheReadTokens: 0,
      cacheCreationTokens: 0,
    };
    let finalText = "";

    // Opus 4.7 omits thinking text unless display:"summarized"; on Sonnet the
    // default already returns summarized thinking, so only set it for opus.
    const thinking = config.model.includes("opus")
      ? ({ type: "adaptive", display: "summarized" } as const)
      : ({ type: "adaptive" } as const);

    for (let turn = 0; turn < MAX_TURNS; turn++) {
      if (signal.aborted) throw new Error("run aborted");

      const stream = this.client.messages.stream(
        {
          model: config.model,
          max_tokens: MAX_TOKENS,
          thinking,
          output_config: { effort: config.effort },
          system: [{ type: "text", text: SYSTEM, cache_control: { type: "ephemeral" } }],
          tools: TOOLS,
          messages: withTurnCache(messages),
        },
        { signal },
      );

      for await (const event of stream) {
        if (event.type !== "content_block_delta") continue;
        if (event.delta.type === "text_delta") {
          emit({ type: "message.delta", text: event.delta.text });
        } else if (event.delta.type === "thinking_delta") {
          emit({ type: "reasoning.available", text: event.delta.thinking });
        }
      }

      const message = await stream.finalMessage();
      const u = message.usage;
      usage.inputTokens += u.input_tokens ?? 0;
      usage.outputTokens += u.output_tokens ?? 0;
      usage.cacheReadTokens += u.cache_read_input_tokens ?? 0;
      usage.cacheCreationTokens += u.cache_creation_input_tokens ?? 0;

      messages.push({ role: "assistant", content: message.content });

      const text = message.content
        .filter((b): b is Anthropic.TextBlock => b.type === "text")
        .map((b) => b.text)
        .join("");
      if (text) finalText = text;

      if (message.stop_reason === "pause_turn") continue;

      const toolUses = message.content.filter(
        (b): b is Anthropic.ToolUseBlock => b.type === "tool_use",
      );
      if (toolUses.length === 0) {
        emit({ type: "run.completed", output: finalText });
        return { output: finalText, usage };
      }

      const results: Anthropic.ToolResultBlockParam[] = [];
      for (const tu of toolUses) {
        emit({ type: "tool.started", tool: tu.name, preview: previewOf(tu) });
        const started = Date.now();
        let isError = false;
        let out = "";
        try {
          out = await execTool(tu, sandbox, emit);
        } catch (e) {
          isError = true;
          out = `error: ${(e as Error).message}`;
        }
        emit({
          type: "tool.completed",
          tool: tu.name,
          duration_s: (Date.now() - started) / 1000,
          error: isError,
        });
        results.push({ type: "tool_result", tool_use_id: tu.id, content: out, is_error: isError });
      }
      messages.push({ role: "user", content: results });
    }

    emit({ type: "run.failed", error: `exceeded ${MAX_TURNS} turns` });
    return { output: finalText, usage };
  }
}

/**
 * Cache the growing prefix: mark the last content block of the most recent
 * message with cache_control so each turn reuses the prior conversation. The
 * frozen system block carries the other breakpoint (which also caches tools,
 * since tools render before system).
 */
function withTurnCache(messages: Anthropic.MessageParam[]): Anthropic.MessageParam[] {
  if (messages.length === 0) return messages;
  const out = messages.slice();
  const last = out[out.length - 1]!;
  const content: Anthropic.ContentBlockParam[] =
    typeof last.content === "string" ? [{ type: "text", text: last.content }] : last.content.slice();
  const lastBlock = content[content.length - 1];
  if (lastBlock) {
    // Clone the block rather than mutating the shared reference — otherwise the
    // breakpoint persists on every prior turn's block and they accumulate past
    // the 4-cache_control limit. This moves the single breakpoint to the latest
    // turn (system carries the other one).
    content[content.length - 1] = {
      ...lastBlock,
      cache_control: { type: "ephemeral" },
    } as Anthropic.ContentBlockParam;
  }
  out[out.length - 1] = { ...last, content };
  return out;
}

function previewOf(tu: Anthropic.ToolUseBlock): string {
  const input = tu.input as Record<string, unknown>;
  if (tu.name === "bash") return String(input.command ?? "").slice(0, 120);
  if (typeof input.path === "string") return input.path;
  return "";
}

async function execTool(
  tu: Anthropic.ToolUseBlock,
  sandbox: Sandbox,
  emit: (e: GatewayEvent) => void,
): Promise<string> {
  const input = tu.input as Record<string, unknown>;
  switch (tu.name) {
    case "read_file":
      return sandbox.readFile(String(input.path));
    case "write_file": {
      const content = String(input.content ?? "");
      await sandbox.writeFile(String(input.path), content);
      const bytes = Buffer.byteLength(content);
      emit({ type: "file.written", path: String(input.path), bytes });
      return `wrote ${bytes} bytes to ${input.path}`;
    }
    case "edit_file": {
      const path = String(input.path);
      const oldStr = String(input.old_string);
      const newStr = String(input.new_string);
      const existing = await sandbox.readFile(path);
      const first = existing.indexOf(oldStr);
      if (first === -1) throw new Error(`old_string not found in ${path}`);
      if (existing.indexOf(oldStr, first + oldStr.length) !== -1) {
        throw new Error(`old_string is not unique in ${path}`);
      }
      const updated = existing.slice(0, first) + newStr + existing.slice(first + oldStr.length);
      await sandbox.writeFile(path, updated);
      emit({ type: "file.written", path, bytes: Buffer.byteLength(updated) });
      return `edited ${path}`;
    }
    case "bash": {
      const r = await sandbox.exec(String(input.command));
      return `exit=${r.exitCode}\n--- stdout ---\n${r.stdout}\n--- stderr ---\n${r.stderr}`;
    }
    default:
      throw new Error(`unknown tool: ${tu.name}`);
  }
}
