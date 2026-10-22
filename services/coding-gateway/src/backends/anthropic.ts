import Anthropic from '@anthropic-ai/sdk';
import {
  defaultDurationSecs,
  workspaceCount,
  ComputeSession,
  MIN_DURATION_SECS,
  type ComputeConfig,
  type ComputeCleanupReport,
} from '../compute.js';
import { config, PRICING } from '../config.js';
import type { GpuRunner } from '../gpu-run.js';
import { isolatedShellCommand } from '../sandbox-command.js';
import type { CodingBackend, GatewayEvent, Sandbox, TokenUsage } from '../types.js';

const MAX_TOKENS = 8_192;
const MAX_TURNS = 30;
const CLAUDE_MODELS = new Set(['claude-opus-4-7', 'claude-sonnet-4-6', 'claude-haiku-4-5']);

type AnthropicClient = Pick<Anthropic, 'messages'>;
type RunOptions = Parameters<CodingBackend['run']>[0];
type ModelPrice = { input: number; output: number; cacheRead: number; cacheWrite: number };

function priceFor(model: string): ModelPrice {
  const price = Object.hasOwn(PRICING, model) ? PRICING[model] : undefined;
  if (
    !CLAUDE_MODELS.has(model) ||
    !price ||
    Object.values(price).some((p) => !Number.isFinite(p) || p <= 0)
  ) {
    throw new Error('Anthropic model has no recognized, valid configured price');
  }
  return price;
}

function tokenCount(value: unknown): number {
  if (typeof value !== 'number' || !Number.isSafeInteger(value) || value < 0)
    throw new Error('Anthropic returned invalid token accounting');
  return value;
}

function usageMicros(usage: TokenUsage, price: ModelPrice): number {
  return Math.ceil(
    usage.inputTokens * price.input +
      usage.outputTokens * price.output +
      usage.cacheReadTokens * price.cacheRead +
      usage.cacheCreationTokens * price.cacheWrite,
  );
}

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
      'Rent a dedicated GPU (CUDA 12.4 runtime on Ubuntu 22.04) on the Covenant compute market ' +
      'and run commands on it. This spends real USDC. Across this whole run you may launch ' +
      `${workspaces} for a combined maximum of $${budgetUsd}. Each launch commits its full ` +
      'booking maximum against that budget and cancelling early does not return it, so launch ' +
      'only when the task genuinely needs a GPU. ' +
      `action=launch books the cheapest online GPU for duration_secs (default ${defaultSecs}s, ` +
      `maximum ${cfg.maxDurationSecs}s); the booked time starts once the GPU is ready. ` +
      'action=run executes a shell command on a GPU you launched. It waits for the GPU to finish ' +
      'starting, so call it right after launch instead of polling status. The command runs with ' +
      'bash in a fresh working directory on the GPU machine and has internet access. python3 is ' +
      'first on PATH; install packages with python3 -m pip, and download with python3 because ' +
      'curl may be missing. files copies files from this sandbox into that directory first. ' +
      'outputs lists result files, relative to that directory, to bring back: each returns with ' +
      'its size and SHA-256, small text files return in full, and every output is published ' +
      'with a download link in the final result automatically. The command must finish within ' +
      'the booked time, which installs also use; it is stopped when that time is nearly up. ' +
      'action=status reports a job you launched. action=cancel stops a job and returns its ' +
      'billing receipt; cancel as soon as you are done with the GPU. The gateway also cancels ' +
      'anything still running when the run ends.',
    input_schema: {
      type: 'object',
      properties: {
        action: { type: 'string', enum: ['launch', 'run', 'status', 'cancel'] },
        job_id: {
          type: 'string',
          description: 'Required for run, status and cancel. Must be a job this run launched.',
        },
        duration_secs: {
          type: 'number',
          description: `Booking window for launch, ${MIN_DURATION_SECS} to ${cfg.maxDurationSecs} seconds. Defaults to ${defaultSecs}.`,
        },
        command: {
          type: 'string',
          description: 'For run: the shell command to execute on the GPU machine.',
        },
        files: {
          type: 'array',
          items: { type: 'string' },
          description:
            'For run: up to 8 sandbox files (4 MB in total) to copy into the working directory, keeping their relative paths.',
        },
        outputs: {
          type: 'array',
          items: { type: 'string' },
          description:
            'For run: up to 8 files the command writes, relative to the working directory, to bring back (32 MB in total).',
        },
        timeout_secs: {
          type: 'number',
          description:
            'For run: stop the command after this many seconds if the booking allows longer.',
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
  private readonly client: AnthropicClient;

  constructor(apiKey?: string, client?: AnthropicClient) {
    // A hidden SDK retry can be another billed request. The gateway records and
    // bounds each generation attempt explicitly; retrying happens at that layer.
    this.client = client ?? new Anthropic({ ...(apiKey ? { apiKey } : {}), maxRetries: 0 });
  }

  async run(opts: RunOptions): Promise<{ output: string; usage: TokenUsage }> {
    const { input, sandbox, signal, emit } = opts;
    // A server-held token is not per-run consent. Only the server can construct
    // and supply this session after validating the explicit dispatch grant.
    if (Boolean(opts.computeSession) !== Boolean(opts.computeConfig)) {
      throw new Error('GPU session and per-run configuration must be supplied together');
    }
    const compute = opts.computeSession ?? null;
    const gpuRunner = compute ? (opts.gpuRunner ?? null) : null;
    const tools = toolsFor(compute ? opts.computeConfig! : null);
    const price = priceFor(config.model);
    const budgetMicros = Math.floor(opts.maxProviderCostUsd * 1_000_000);
    if (!Number.isSafeInteger(budgetMicros) || budgetMicros <= 0) {
      throw new Error('Anthropic provider budget must be finite and positive');
    }
    let spentMicros = 0;
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

    try {
      for (let turn = 0; turn < MAX_TURNS; turn++) {
        if (signal.aborted) throw new Error('run aborted');

        const request = {
          model: config.model,
          thinking,
          system: [
            { type: 'text', text: SYSTEM, cache_control: { type: 'ephemeral' } },
          ] as Anthropic.TextBlockParam[],
          tools,
          messages: withTurnCache(messages),
        };
        // Counting is non-generative. Price the entire prompt at the worst
        // configured input/cache rate, including tool schemas and transcript.
        // Token counts are estimates: leave 10% + 1024 input tokens of headroom.
        const counted = await this.client.messages.countTokens(request, { signal, maxRetries: 0 });
        const inputTokens = tokenCount(counted.input_tokens);
        const reservedInput = Math.ceil(inputTokens * 1.1) + 1_024;
        const inputMicros = Math.ceil(
          reservedInput * Math.max(price.input, price.cacheWrite, price.cacheRead),
        );
        const maxTokens = Math.min(
          MAX_TOKENS,
          Math.floor((budgetMicros - spentMicros - inputMicros) / price.output),
        );
        if (!Number.isSafeInteger(maxTokens) || maxTokens < 1)
          throw new Error('Anthropic provider budget exhausted before generation');
        if (signal.aborted) throw new Error('run aborted');
        await opts.recordProviderRequest?.();
        if (signal.aborted) throw new Error('run aborted');
        const stream = this.client.messages.stream(
          { ...request, max_tokens: maxTokens, output_config: { effort: config.effort } },
          { signal, maxRetries: 0 },
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
        const turnUsage: TokenUsage = {
          inputTokens: tokenCount(u.input_tokens),
          outputTokens: tokenCount(u.output_tokens),
          cacheReadTokens: tokenCount(u.cache_read_input_tokens ?? 0),
          cacheCreationTokens: tokenCount(u.cache_creation_input_tokens ?? 0),
        };
        usage.inputTokens += turnUsage.inputTokens;
        usage.outputTokens += turnUsage.outputTokens;
        usage.cacheReadTokens += turnUsage.cacheReadTokens;
        usage.cacheCreationTokens += turnUsage.cacheCreationTokens;
        spentMicros += usageMicros(turnUsage, price);
        if (spentMicros > budgetMicros)
          throw new Error('Anthropic usage exceeded the configured provider reservation');
        if (signal.aborted) throw new Error('run aborted');

        messages.push({ role: 'assistant', content: message.content });

        const text = message.content
          .filter((b): b is Anthropic.TextBlock => b.type === 'text')
          .map((b) => b.text)
          .join('');
        if (text) finalText = text;

        if (message.stop_reason === 'max_tokens')
          throw new Error('Anthropic output token budget exhausted');
        if (message.stop_reason === 'pause_turn') continue;

        const toolUses = message.content.filter(
          (b): b is Anthropic.ToolUseBlock => b.type === 'tool_use',
        );
        if (toolUses.length === 0) {
          // The server owns cleanup, receipts, charging, and terminal events.
          return { output: finalText, usage };
        }

        const results: Anthropic.ToolResultBlockParam[] = [];
        for (const tu of toolUses) {
          // Outside the tool-error catch: cancellation must stop dispatch,
          // not become a recoverable tool result that allows a later launch.
          if (signal.aborted) throw new Error('run aborted');
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
            out = await execTool(tu, sandbox, emit, compute, gpuRunner);
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

      throw new Error(`exceeded ${MAX_TURNS} Anthropic turns`);
    } catch (error) {
      // Complete prior usage is useful evidence, but it cannot settle an
      // interrupted provider request. The server retains that reservation.
      const failure = error instanceof Error ? error : new Error('Anthropic run failed');
      Object.assign(failure, { usage: { ...usage } });
      throw failure;
    }
  }
}

/** Format an existing cleanup report; never perform cleanup from a backend. */
export function reapNote(report: ComputeCleanupReport | null): string {
  if (!report || (report.settled.length === 0 && report.complete)) return '';
  if (!report.complete)
    return '\n[gpu_workspace cleanup pending; final receipts are not confirmed]';
  return `\n[gpu_workspace settled at run end: ${report.settled.map((job) => job.job_id).join(', ')}]`;
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
  const parts = [action, `job=${String(job.job_id ?? '?')}`];
  if (action === 'run') {
    parts.push(`exit=${String(job.exit_code ?? 'none')}`);
    if (job.timed_out === true) parts.push('timed_out');
    if (Array.isArray(job.outputs)) parts.push(`outputs=${job.outputs.length}`);
    return parts.join(' ');
  }
  parts.push(`status=${String(job.status ?? '?')}`);
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
  gpuRunner: GpuRunner | null = null,
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
      if (action === 'run') {
        if (!gpuRunner) throw new Error('running commands on a GPU is not enabled on this gateway');
        return gpuRunner.run(input, sandbox);
      }
      if (action !== 'status' && action !== 'cancel') {
        throw new Error(
          `gpu_workspace action must be launch, run, status, or cancel: got ${action}`,
        );
      }
      const jobId = String(input.job_id ?? '');
      if (!jobId) throw new Error('job_id is required');
      const job = action === 'cancel' ? await compute.cancel(jobId) : await compute.status(jobId);
      // The workspace address carries a live token and stays inside the gateway.
      return JSON.stringify({
        job_id: job.id,
        status: job.status,
        ready: job.status === 'running' && Boolean(job.access_url),
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
