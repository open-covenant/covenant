import Anthropic from '@anthropic-ai/sdk';
import {
  computeConfig,
  defaultDurationSecs,
  workspaceCount,
  ComputeSession,
  MIN_DURATION_SECS,
  type ComputeConfig,
} from '../compute.js';
import { config } from '../config.js';
import { isolatedShellCommand } from '../sandbox-command.js';
import type { CodingBackend, GatewayEvent, Sandbox, TokenUsage } from '../types.js';

const MAX_TOKENS = 64_000;
const MAX_TURNS = 60;

const SYSTEM = `You are a coding agent working inside an ephemeral sandbox with a
few-minute wall-clock budget, so work efficiently and don't waste steps.
Build what the user asks: create and edit files, run commands, install
dependencies, and verify your work by running it. The working directory is the
workspace root; use relative paths. Prefer small, verifiable steps: write a
file, run it, read the output, fix.
Be fast with dependencies: scaffold without redundant installs (e.g.
\`create-next-app --skip-install\` then a single install), prefer \`pnpm\` when
available. A clean typecheck (or a successful build) is enough to verify; you
don't need a full production build unless asked.
NEVER run a long-lived or blocking command directly: a dev server, \`npm run
dev\`, a file watcher, \`npm start\`. It never exits, so it hangs your session
until it times out and burns your whole budget. To check a server, bound it:
\`timeout 8 npm run dev\` (or background it, sleep, curl, then kill it). To
verify a web app, prefer a typecheck/build over starting a server at all.
If an install looks broken, reinstall cleanly (\`rm -rf node_modules\` then
install). Never hand-delete files inside node_modules.
Finish with a short summary of what you built and how to run it.`;

const TOOLS: Anthropic.Tool[] = [
  {
    name: 'read_file',
    description: 'Read a UTF-8 file from the workspace.',
    input_schema: {
      type: 'object',
      properties: { path: { type: 'string', description: 'Path relative to the workspace root' } },
      required: ['path'],
    },
  },
  {
    name: 'write_file',
    description: 'Create or overwrite a UTF-8 file in the workspace.',
    input_schema: {
      type: 'object',
      properties: {
        path: { type: 'string', description: 'Path relative to the workspace root' },
        content: { type: 'string', description: 'Full file contents' },
      },
      required: ['path', 'content'],
    },
  },
  {
    name: 'edit_file',
    description:
      'Replace an exact, unique substring in an existing file. Fails if old_string is missing or appears more than once.',
    input_schema: {
      type: 'object',
      properties: {
        path: { type: 'string' },
        old_string: { type: 'string', description: 'Exact text to replace' },
        new_string: { type: 'string', description: 'Replacement text' },
      },
      required: ['path', 'old_string', 'new_string'],
    },
  },
  {
    name: 'bash',
    description: 'Run a shell command in the workspace and return stdout, stderr, and exit code.',
    input_schema: {
      type: 'object',
      properties: { command: { type: 'string' } },
      required: ['command'],
    },
  },
];

export function gpuWorkspaceTool(cfg: ComputeConfig): Anthropic.Tool {
  const budgetUsd = (cfg.maxUsdcMicros / 1_000_000).toFixed(2);
  const defaultSecs = defaultDurationSecs(cfg);
  const workspaces = workspaceCount(cfg.maxLaunches);
  return {
    name: 'gpu_workspace',
    description:
      'Rent a dedicated GPU workspace (CUDA + Jupyter) on the Covenant compute market. This spends ' +
      `real USDC. Across this whole run you may launch ${workspaces} for a combined maximum of ` +
      `$${budgetUsd}. Each launch commits its full booking maximum against that budget and ` +
      'cancelling early does not return it, so launch only when the task genuinely needs a GPU. ' +
      `action=launch books the cheapest online GPU for duration_secs (default ${defaultSecs}s, ` +
      `maximum ${cfg.maxDurationSecs}s). action=status polls a job you launched: provisioning takes ` +
      'minutes and access_url appears once it is running. Every poll costs a full model turn, so ' +
      'run `sleep 25` with the bash tool between polls instead of polling back to back. ' +
      'action=cancel stops a job and returns its billing receipt. You cannot use the workspace ' +
      'yourself. This sandbox can only reach a short list of package registries, so nothing you ' +
      'run here can connect to the access URL. Treat access_url as a live credential for the ' +
      'person who asked for the workspace: put it in your final answer once and do not repeat it ' +
      'elsewhere. The gateway tries to cancel anything still running when the run ends, but a ' +
      'workspace that will not cancel bills until its own deadline.',
    input_schema: {
      type: 'object',
      properties: {
        action: { type: 'string', enum: ['launch', 'status', 'cancel'] },
        job_id: {
          type: 'string',
          description: 'Required for status and cancel. Must be a job this run launched.',
        },
        duration_secs: {
          type: 'number',
          description: `Booking window for launch, ${MIN_DURATION_SECS} to ${cfg.maxDurationSecs} seconds. Defaults to ${defaultSecs}.`,
        },
      },
      required: ['action'],
    },
  };
}

export function toolsFor(cfg: ComputeConfig | null): Anthropic.Tool[] {
  return cfg ? [...TOOLS, gpuWorkspaceTool(cfg)] : TOOLS;
}

export class AnthropicBackend implements CodingBackend {
  readonly id = 'anthropic' as const;
  private readonly client: Anthropic;

  constructor(apiKey?: string) {
    this.client = apiKey ? new Anthropic({ apiKey }) : new Anthropic();
  }

  async run(opts: {
    input: string;
    sandbox: Sandbox;
    signal: AbortSignal;
    emit: (e: GatewayEvent) => void;
    recordComputeUsd?: (usd: number) => void;
  }): Promise<{ output: string; usage: TokenUsage }> {
    const { input, sandbox, signal, emit } = opts;
    const compute = computeConfig ? new ComputeSession(computeConfig) : null;
    const tools = toolsFor(computeConfig);
    const messages: Anthropic.MessageParam[] = [{ role: 'user', content: input }];
    const usage: TokenUsage = {
      inputTokens: 0,
      outputTokens: 0,
      cacheReadTokens: 0,
      cacheCreationTokens: 0,
    };
    let finalText = '';

    // Opus 4.7 omits thinking text unless display:"summarized"; on Sonnet the
    // default already returns summarized thinking, so only set it for opus.
    const thinking = config.model.includes('opus')
      ? ({ type: 'adaptive', display: 'summarized' } as const)
      : ({ type: 'adaptive' } as const);

    // Runs before the terminal event on every path, so a reaped workspace is
    // named inside the run's output and not after it. The finally calls this
    // again to catch an abort; once the run has ended a late retry still reaps,
    // but stays silent so nothing is emitted past the terminal event.
    let ended = false;
    const settleCompute = async (): Promise<void> => {
      const note = await reapNote(compute);
      if (!note || ended) return;
      emit({ type: 'message.delta', text: note });
      finalText += note;
    };

    try {
      for (let turn = 0; turn < MAX_TURNS; turn++) {
        if (signal.aborted) throw new Error('run aborted');

        const stream = this.client.messages.stream(
          {
            model: config.model,
            max_tokens: MAX_TOKENS,
            thinking,
            output_config: { effort: config.effort },
            system: [{ type: 'text', text: SYSTEM, cache_control: { type: 'ephemeral' } }],
            tools,
            messages: withTurnCache(messages),
          },
          { signal },
        );

        for await (const event of stream) {
          if (event.type !== 'content_block_delta') continue;
          if (event.delta.type === 'text_delta') {
            emit({ type: 'message.delta', text: event.delta.text });
          } else if (event.delta.type === 'thinking_delta') {
            emit({ type: 'reasoning.available', text: event.delta.thinking });
          }
        }

        const message = await stream.finalMessage();
        const u = message.usage;
        usage.inputTokens += u.input_tokens ?? 0;
        usage.outputTokens += u.output_tokens ?? 0;
        usage.cacheReadTokens += u.cache_read_input_tokens ?? 0;
        usage.cacheCreationTokens += u.cache_creation_input_tokens ?? 0;

        messages.push({ role: 'assistant', content: message.content });

        const text = message.content
          .filter((b): b is Anthropic.TextBlock => b.type === 'text')
          .map((b) => b.text)
          .join('');
        if (text) finalText = text;

        if (message.stop_reason === 'pause_turn') continue;

        const toolUses = message.content.filter(
          (b): b is Anthropic.ToolUseBlock => b.type === 'tool_use',
        );
        if (toolUses.length === 0) {
          await settleCompute();
          ended = true;
          emit({ type: 'run.completed', output: finalText });
          return { output: finalText, usage };
        }

        const results: Anthropic.ToolResultBlockParam[] = [];
        for (const tu of toolUses) {
          // The daemon carries `preview` on tool.started only, and what an
          // operator needs from a GPU call (job, offer, booked maximum, receipt)
          // exists only after it returns. So the GPU frame is emitted late, still
          // ahead of its tool.completed; every other tool keeps its live preview.
          const late = tu.name === 'gpu_workspace';
          if (!late) emit({ type: 'tool.started', tool: tu.name, preview: previewOf(tu) });
          const started = Date.now();
          let isError = false;
          let out = '';
          try {
            out = await execTool(tu, sandbox, emit, compute);
          } catch (e) {
            isError = true;
            out = `error: ${(e as Error).message}`;
          }
          if (late) {
            emit({
              type: 'tool.started',
              tool: tu.name,
              preview: computePreview(tu.input as Record<string, unknown>, out, isError),
            });
          }
          emit({
            type: 'tool.completed',
            tool: tu.name,
            duration_s: (Date.now() - started) / 1000,
            error: isError,
          });
          results.push({
            type: 'tool_result',
            tool_use_id: tu.id,
            content: out,
            is_error: isError,
          });
        }
        messages.push({ role: 'user', content: results });
      }

      await settleCompute();
      ended = true;
      emit({ type: 'run.failed', error: `exceeded ${MAX_TURNS} turns` });
      return { output: finalText, usage };
    } finally {
      // Success, failure, or abort: never leak a billed workspace past the run.
      await settleCompute();
      const committed = compute?.committedUsd() ?? 0;
      if (committed > 0) {
        console.log(`gpu_workspace committed $${committed.toFixed(4)}`);
        // Reported here rather than through the return value so an aborted or
        // failed run still charges the GPU spend it already committed.
        opts.recordComputeUsd?.(committed);
      }
    }
  }
}

export async function reapNote(compute: ComputeSession | null): Promise<string> {
  if (!compute) return '';
  const reaped = await compute.reap();
  if (reaped.length === 0) return '';
  return `\n[gpu_workspace cancelled at run end: ${reaped.join(', ')}]`;
}

/**
 * Cache the growing prefix: mark the last content block of the most recent
 * message with cache_control so each turn reuses the prior conversation. The
 * frozen system block carries the other breakpoint (which also caches tools,
 * since tools render before system).
 */
export function withTurnCache(messages: Anthropic.MessageParam[]): Anthropic.MessageParam[] {
  if (messages.length === 0) return messages;
  const out = messages.slice();
  const last = out[out.length - 1]!;
  const content: Anthropic.ContentBlockParam[] =
    typeof last.content === 'string'
      ? [{ type: 'text', text: last.content }]
      : last.content.slice();
  const lastBlock = content[content.length - 1];
  if (lastBlock) {
    // Clone the block instead of mutating the shared reference. Mutating leaves
    // the breakpoint on every prior turn's block, and they accumulate past the
    // 4-cache_control limit; cloning moves the single breakpoint to the latest
    // turn (system carries the other one).
    content[content.length - 1] = {
      ...lastBlock,
      cache_control: { type: 'ephemeral' },
    } as Anthropic.ContentBlockParam;
  }
  out[out.length - 1] = { ...last, content };
  return out;
}

/** A model that sends "600" means 600 seconds; the session rejects the rest. */
function requestedDuration(raw: unknown): number | undefined {
  if (raw === undefined || raw === null || raw === '') return undefined;
  return Number(raw);
}

export function previewOf(tu: Anthropic.ToolUseBlock): string {
  const input = tu.input as Record<string, unknown>;
  if (tu.name === 'bash') return String(input.command ?? '').slice(0, 120);
  if (typeof input.path === 'string') return input.path;
  return '';
}

/**
 * Audit line for one GPU call: what was booked and what it cost. Never
 * access_url, which is a live credential for the workspace.
 */
export function computePreview(
  input: Record<string, unknown>,
  result: string,
  failed: boolean,
): string {
  const action = String(input.action ?? 'unknown');
  if (failed) return `${action} failed: ${result.replace(/^error: /, '')}`.slice(0, 200);

  let job: Record<string, unknown>;
  try {
    job = JSON.parse(result) as Record<string, unknown>;
  } catch {
    return action;
  }
  const parts = [action, `job=${String(job.job_id ?? '?')}`, `status=${String(job.status ?? '?')}`];
  if (job.offer_id !== undefined) parts.push(`offer=${String(job.offer_id)}`);
  if (job.maximum_usdc_micros !== undefined) {
    parts.push(`booked_max_usdc_micros=${String(job.maximum_usdc_micros)}`);
  }
  const receipt = job.receipt as Record<string, unknown> | null | undefined;
  if (receipt) {
    parts.push(`charged_usdc_micros=${String(receipt.charged_usdc_micros)}`);
    parts.push(`refunded_usdc_micros=${String(receipt.refunded_usdc_micros)}`);
  }
  return parts.join(' ');
}

export async function execTool(
  tu: Anthropic.ToolUseBlock,
  sandbox: Sandbox,
  emit: (e: GatewayEvent) => void,
  compute: ComputeSession | null = null,
): Promise<string> {
  const input = tu.input as Record<string, unknown>;
  switch (tu.name) {
    case 'gpu_workspace': {
      if (!compute) throw new Error('gpu_workspace is not enabled on this gateway');
      const action = String(input.action);
      if (action === 'launch') {
        const job = await compute.launch(requestedDuration(input.duration_secs));
        return JSON.stringify({
          job_id: job.id,
          status: job.status,
          offer_id: job.offer_id,
          maximum_usdc_micros: job.maximum_usdc_micros,
        });
      }
      if (action !== 'status' && action !== 'cancel') {
        throw new Error(`gpu_workspace action must be launch, status, or cancel: got ${action}`);
      }
      const jobId = String(input.job_id ?? '');
      if (!jobId) throw new Error('job_id is required');
      const job = action === 'cancel' ? await compute.cancel(jobId) : await compute.status(jobId);
      return JSON.stringify({
        job_id: job.id,
        status: job.status,
        access_url: job.access_url,
        error: job.error,
        receipt: job.receipt,
      });
    }
    case 'read_file':
      return sandbox.readFile(String(input.path));
    case 'write_file': {
      const content = String(input.content ?? '');
      await sandbox.writeFile(String(input.path), content);
      const bytes = Buffer.byteLength(content);
      emit({ type: 'file.written', path: String(input.path), bytes });
      return `wrote ${bytes} bytes to ${input.path}`;
    }
    case 'edit_file': {
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
      emit({ type: 'file.written', path, bytes: Buffer.byteLength(updated) });
      return `edited ${path}`;
    }
    case 'bash': {
      const r = await sandbox.exec(isolatedShellCommand(String(input.command)));
      return `exit=${r.exitCode}\n--- stdout ---\n${r.stdout}\n--- stderr ---\n${r.stderr}`;
    }
    default:
      throw new Error(`unknown tool: ${tu.name}`);
  }
}
