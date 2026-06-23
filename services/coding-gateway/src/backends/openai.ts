import OpenAI from "openai";
import { config } from "../config.js";
import type { CodingBackend, GatewayEvent, Sandbox, TokenUsage } from "../types.js";
import type {
  FunctionTool,
  Response as OAIResponse,
  ResponseFunctionToolCall,
  ResponseInputItem,
  ResponseStreamEvent,
} from "openai/resources/responses/responses.js";
import type { ReasoningEffort } from "openai/resources/shared.js";

const MAX_OUTPUT_TOKENS = 64_000;
const MAX_TURNS = 60;

const MODEL = process.env.CODER_OPENAI_MODEL ?? "gpt-5-codex";

const SYSTEM = `You are a coding agent working inside an ephemeral sandbox with a
few-minute wall-clock budget, so work efficiently and don't waste steps.
Build what the user asks: create and edit files, run commands, install
dependencies, and verify your work by running it. The working directory is the
workspace root; use relative paths. Prefer small, verifiable steps — write a
file, run it, read the output, fix.
Be fast with dependencies: scaffold without redundant installs (e.g.
\`create-next-app --skip-install\` then a single install), prefer \`pnpm\` when
available. A clean typecheck (or a successful build) is enough to verify — you
don't need a full production build unless asked.
NEVER run a long-lived or blocking command directly — a dev server, \`npm run
dev\`, a file watcher, \`npm start\`. It never exits, so it hangs your session
until it times out and burns your whole budget. To check a server, bound it:
\`timeout 8 npm run dev\` (or background it, sleep, curl, then kill it). To
verify a web app, prefer a typecheck/build over starting a server at all.
If an install looks broken, reinstall cleanly (\`rm -rf node_modules\` then
install) — never hand-delete files inside node_modules.
Finish with a short summary of what you built and how to run it.`;

const TOOLS: FunctionTool[] = [
  {
    type: "function",
    name: "read_file",
    description: "Read a UTF-8 file from the workspace.",
    strict: false,
    parameters: {
      type: "object",
      properties: { path: { type: "string", description: "Path relative to the workspace root" } },
      required: ["path"],
    },
  },
  {
    type: "function",
    name: "write_file",
    description: "Create or overwrite a UTF-8 file in the workspace.",
    strict: false,
    parameters: {
      type: "object",
      properties: {
        path: { type: "string", description: "Path relative to the workspace root" },
        content: { type: "string", description: "Full file contents" },
      },
      required: ["path", "content"],
    },
  },
  {
    type: "function",
    name: "edit_file",
    description:
      "Replace an exact, unique substring in an existing file. Fails if old_string is missing or appears more than once.",
    strict: false,
    parameters: {
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
    type: "function",
    name: "bash",
    description: "Run a shell command in the workspace and return stdout, stderr, and exit code.",
    strict: false,
    parameters: {
      type: "object",
      properties: { command: { type: "string" } },
      required: ["command"],
    },
  },
];

export class OpenaiBackend implements CodingBackend {
  readonly id = "openai" as const;
  private readonly injectedClient: OpenAI | undefined;
  private readonly apiKey: string | undefined;
  private _client?: OpenAI;

  // Production calls this with no argument; the client is built lazily on first
  // run so a missing OPENAI_API_KEY errors at run time, not at backend selection
  // — matching the Anthropic adapter, whose SDK doesn't validate credentials at
  // construction. Tests inject a fake client to drive the loop without a key.
  constructor(clientOrApiKey?: OpenAI | string) {
    if (typeof clientOrApiKey === "string") this.apiKey = clientOrApiKey;
    else if (clientOrApiKey) this.injectedClient = clientOrApiKey;
  }

  private get client(): OpenAI {
    if (!this._client) {
      this._client = this.injectedClient ?? (this.apiKey ? new OpenAI({ apiKey: this.apiKey }) : new OpenAI());
    }
    return this._client;
  }

  async run(opts: {
    input: string;
    sandbox: Sandbox;
    signal: AbortSignal;
    emit: (e: GatewayEvent) => void;
  }): Promise<{ output: string; usage: TokenUsage }> {
    const { input, sandbox, signal, emit } = opts;
    const usage: TokenUsage = {
      inputTokens: 0,
      outputTokens: 0,
      cacheReadTokens: 0,
      cacheCreationTokens: 0,
    };
    let finalText = "";

    // Multi-turn is driven by previous_response_id: the API retains the prior
    // turns server-side, so each follow-up only carries the tool outputs.
    let previousResponseId: string | undefined;
    let nextInput: ResponseInputItem[] = [{ role: "user", content: input }];

    for (let turn = 0; turn < MAX_TURNS; turn++) {
      if (signal.aborted) throw new Error("run aborted");

      const stream = await this.client.responses.create(
        {
          model: MODEL,
          input: nextInput,
          instructions: SYSTEM,
          tools: TOOLS,
          max_output_tokens: MAX_OUTPUT_TOKENS,
          reasoning: { effort: reasoningEffort(config.effort) },
          previous_response_id: previousResponseId,
          stream: true,
        },
        { signal },
      );

      let response: OAIResponse | undefined;
      for await (const event of stream) {
        switch (event.type) {
          case "response.output_text.delta":
            emit({ type: "message.delta", text: event.delta });
            break;
          case "response.reasoning_summary_text.delta":
            emit({ type: "reasoning.available", text: event.delta });
            break;
          case "response.completed":
            response = event.response;
            break;
          case "response.failed":
            // Surface the API's error; the run contract is that a failed
            // response throws rather than emitting run.failed, matching how a
            // budget abort or transport error would propagate.
            throw new Error(responseErrorMessage(event.response) ?? "openai response failed");
        }
      }
      if (!response) throw new Error("openai stream ended without a completed response");

      const u = response.usage;
      if (u) {
        usage.inputTokens += u.input_tokens ?? 0;
        usage.outputTokens += u.output_tokens ?? 0;
        usage.cacheReadTokens += u.input_tokens_details?.cached_tokens ?? 0;
      }

      const text = response.output
        .filter(isMessageItem)
        .flatMap((m) => m.content)
        .map((c) => (c.type === "output_text" ? c.text : ""))
        .join("");
      if (text) finalText = text;

      const toolCalls = response.output.filter(
        (o): o is ResponseFunctionToolCall => o.type === "function_call",
      );
      if (toolCalls.length === 0) {
        emit({ type: "run.completed", output: finalText });
        return { output: finalText, usage };
      }

      const outputs: ResponseInputItem.FunctionCallOutput[] = [];
      for (const call of toolCalls) {
        emit({ type: "tool.started", tool: call.name, preview: previewOf(call) });
        const started = Date.now();
        let isError = false;
        let out = "";
        try {
          out = await execTool(call, sandbox, emit);
        } catch (e) {
          isError = true;
          out = `error: ${(e as Error).message}`;
        }
        emit({
          type: "tool.completed",
          tool: call.name,
          duration_s: (Date.now() - started) / 1000,
          error: isError,
        });
        outputs.push({ type: "function_call_output", call_id: call.call_id, output: out });
      }

      previousResponseId = response.id;
      nextInput = outputs;
    }

    emit({ type: "run.failed", error: `exceeded ${MAX_TURNS} turns` });
    return { output: finalText, usage };
  }
}

// The shared gateway effort dial maps straight onto OpenAI's reasoning effort;
// only "max" (a Claude-tier alias) has no OpenAI equivalent and is clamped down.
export function reasoningEffort(effort: string): ReasoningEffort {
  if (effort === "max") return "xhigh";
  return effort as ReasoningEffort;
}

function previewOf(call: ResponseFunctionToolCall): string {
  const input = parseArgs(call);
  if (call.name === "bash") return String(input.command ?? "").slice(0, 120);
  if (typeof input.path === "string") return input.path;
  return "";
}

async function execTool(
  call: ResponseFunctionToolCall,
  sandbox: Sandbox,
  emit: (e: GatewayEvent) => void,
): Promise<string> {
  const input = parseArgs(call);
  switch (call.name) {
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
      throw new Error(`unknown tool: ${call.name}`);
  }
}

function parseArgs(call: ResponseFunctionToolCall): Record<string, unknown> {
  try {
    return call.arguments ? (JSON.parse(call.arguments) as Record<string, unknown>) : {};
  } catch {
    return {};
  }
}

function responseErrorMessage(response: OAIResponse): string | undefined {
  const err = response.error as { message?: string; code?: string } | null | undefined;
  if (!err) return undefined;
  return err.code ? `${err.code}: ${err.message ?? ""}`.trim() : err.message;
}

type ResponseOutputItem = OAIResponse["output"][number];
type ResponseMessageItem = Extract<ResponseOutputItem, { type: "message" }>;

function isMessageItem(o: ResponseOutputItem): o is ResponseMessageItem {
  return o.type === "message";
}
