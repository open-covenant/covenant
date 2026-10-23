import { basename } from 'node:path';
import { randomUUID } from 'node:crypto';
import type { ComputeSession } from './compute.js';
import { ARTIFACT_NAME, type ArtifactStore } from './artifacts.js';
import type { Sandbox } from './types.js';
import {
  MAX_COMMAND_BYTES,
  MAX_OUTPUT_FILES,
  MAX_UPLOAD_BYTES,
  MAX_UPLOAD_FILES,
  WorkspaceClient,
  WorkspaceError,
  parseWorkspaceUrl,
  runWorkspaceJob,
  safeRelativePath,
  type WorkspaceJobOutcome,
  type WorkspaceJobProgress,
} from './workspace-exec.js';

/** One file a GPU command produced, as recorded on the run and shown in its result. */
export interface GpuOutput {
  job_id: string;
  path: string;
  bytes: number;
  sha256: string;
  url: string | null;
  expires_at: string;
}

export const MAX_GPU_COMMANDS = 32;
const GPU_FAILURES = [
  'timeout',
  'unknown_exit',
  'nonzero_exit',
  'missing_outputs',
  'oversized_outputs',
  'execution_error',
] as const;
type GpuFailure = (typeof GPU_FAILURES)[number];

/** Durable workload evidence; no commands or workspace credentials, only bounded log tails. */
export interface GpuCommandOutcome {
  id: string;
  job_id: string;
  status: 'pending' | 'succeeded' | 'failed';
  phase: 'waiting_for_compute' | WorkspaceJobProgress['phase'];
  exit_code: number | null;
  timed_out: boolean | null;
  requested_outputs: string[];
  collected_outputs: string[];
  missing_outputs: string[];
  oversized_outputs: string[];
  failure: GpuFailure | null;
  timings: {
    readinessWaitMs: number | null;
    workspaceReadyWaitMs: number | null;
    stagingMs: number | null;
    commandBudgetMs: number | null;
    commandElapsedMs: number | null;
    collectionMs: number | null;
  };
  stdout_tail: string;
  stderr_tail: string;
}

function validPaths(value: unknown): value is string[] {
  if (
    !Array.isArray(value) ||
    value.length > MAX_OUTPUT_FILES ||
    new Set(value).size !== value.length
  )
    return false;
  try {
    return value.every((path) => safeRelativePath(path, 'output') === path);
  } catch {
    return false;
  }
}

export function isGpuCommandOutcome(value: unknown): value is GpuCommandOutcome {
  const v = value as GpuCommandOutcome | null;
  if (
    !v ||
    typeof v !== 'object' ||
    typeof v.id !== 'string' ||
    v.id.length !== 36 ||
    !/^[a-f0-9]{8}-[a-f0-9]{4}-[a-f0-9]{4}-[a-f0-9]{4}-[a-f0-9]{12}$/.test(v.id) ||
    typeof v.job_id !== 'string' ||
    v.job_id.length === 0 ||
    v.job_id.length > 128 ||
    /[^A-Za-z0-9:_-]/.test(v.job_id) ||
    !['pending', 'succeeded', 'failed'].includes(v.status) ||
    ![
      'waiting_for_compute',
      'waiting_for_workspace',
      'staging',
      'executing',
      'stopping',
      'collecting',
      'complete',
    ].includes(v.phase) ||
    !(v.exit_code === null || Number.isSafeInteger(v.exit_code)) ||
    !(v.timed_out === null || typeof v.timed_out === 'boolean') ||
    !validPaths(v.requested_outputs) ||
    !validPaths(v.collected_outputs) ||
    !validPaths(v.missing_outputs) ||
    !validPaths(v.oversized_outputs) ||
    !v.timings ||
    [
      'readinessWaitMs',
      'workspaceReadyWaitMs',
      'stagingMs',
      'commandBudgetMs',
      'commandElapsedMs',
      'collectionMs',
    ].some((key) => {
      const n = v.timings[key as keyof GpuCommandOutcome['timings']];
      return (
        n !== null && (typeof n !== 'number' || !Number.isFinite(n) || n < 0 || n > 86_400_000)
      );
    }) ||
    typeof v.stdout_tail !== 'string' ||
    v.stdout_tail.length > TAIL_CHARS ||
    typeof v.stderr_tail !== 'string' ||
    v.stderr_tail.length > TAIL_CHARS ||
    ![...v.collected_outputs, ...v.missing_outputs, ...v.oversized_outputs].every((path) =>
      v.requested_outputs.includes(path),
    )
  )
    return false;
  if (v.status === 'pending')
    return (
      v.exit_code === null &&
      v.timed_out === null &&
      v.failure === null &&
      v.collected_outputs.length + v.missing_outputs.length + v.oversized_outputs.length === 0
    );
  if (v.status === 'succeeded')
    return (
      v.phase === 'complete' &&
      v.exit_code === 0 &&
      v.timed_out === false &&
      v.failure === null &&
      v.missing_outputs.length === 0 &&
      v.oversized_outputs.length === 0 &&
      v.requested_outputs.every((path) => v.collected_outputs.includes(path))
    );
  return GPU_FAILURES.includes(v.failure as GpuFailure);
}

export function gpuWorkloadSummary(commands: readonly GpuCommandOutcome[] = []) {
  const relevant = commands.find((command) => command.status !== 'succeeded') ?? commands.at(-1);
  return {
    // A failed attempt cannot be erased by a later command or model completion.
    status: commands.some((command) => command.status !== 'succeeded')
      ? 'failed'
      : commands.length
        ? 'succeeded'
        : 'not_run',
    commands: commands.length,
    failed: commands.filter((command) => command.status === 'failed').length,
    incomplete: commands.filter((command) => command.status === 'pending').length,
    failures: [
      ...new Set(commands.flatMap((command) => (command.failure ? [command.failure] : []))),
    ],
    outputs_checked: 'command_declared_only',
    ...(relevant
      ? {
          outcome: {
            id: relevant.id,
            job_id: relevant.job_id,
            status: relevant.status,
            phase: relevant.phase,
            exit_code: relevant.exit_code,
            timed_out: relevant.timed_out,
            requested_outputs: relevant.requested_outputs.length,
            collected_outputs: relevant.collected_outputs.length,
            missing_outputs: relevant.missing_outputs.length,
            oversized_outputs: relevant.oversized_outputs.length,
            timings: relevant.timings,
            // Log tails stay in durable/API evidence; escaped control characters
            // can otherwise crowd final receipts out of the bounded transport.
          },
        }
      : {}),
  };
}

/** The backend keeps this structured diagnostic when marking the tool as failed. */
export class GpuWorkloadError extends WorkspaceError {
  constructor(readonly toolResult: string) {
    super('GPU workload did not succeed');
  }
}

// Compute starts the booking clock when the workspace becomes ready, at or
// before the moment this gateway first saw it running. Leave room for that gap
// and for collecting outputs before Compute stops the machine.
const BOOKING_MARGIN_MS = 45_000;
// The run itself still has to settle the GPU and write its result.
const RUN_MARGIN_MS = 45_000;
const MIN_COMMAND_MS = 20_000;
const MAX_READY_WAIT_MS = 240_000;
const PREVIEW_BYTES = 4_096;
const TAIL_CHARS = 2_000;

export function isGpuOutput(value: unknown): value is GpuOutput {
  const v = value as GpuOutput | null;
  return (
    !!v &&
    typeof v === 'object' &&
    typeof v.job_id === 'string' &&
    v.job_id.length <= 128 &&
    typeof v.path === 'string' &&
    v.path.length <= 128 &&
    Number.isSafeInteger(v.bytes) &&
    v.bytes >= 0 &&
    typeof v.sha256 === 'string' &&
    /^[a-f0-9]{64}$/.test(v.sha256) &&
    (v.url === null || (typeof v.url === 'string' && v.url.length <= 512)) &&
    typeof v.expires_at === 'string' &&
    Number.isFinite(Date.parse(v.expires_at))
  );
}

export interface GpuRunnerDeps {
  compute: ComputeSession;
  store: ArtifactStore;
  signal: AbortSignal;
  /** When the gateway aborts this run. */
  runDeadlineMs: number;
  /** Outputs this run already recorded; the per-run cap spans every command. */
  recorded: () => number;
  record: (outputs: GpuOutput[]) => void;
  commands: () => readonly GpuCommandOutcome[];
  recordCommand: (outcome: GpuCommandOutcome) => void;
  connect?: (
    accessUrl: string,
  ) => Pick<
    WorkspaceClient,
    'waitUntilServing' | 'makeDirectory' | 'upload' | 'download' | 'execute' | 'close'
  >;
  now?: () => number;
  pollMs?: number;
}

export class GpuRunner {
  constructor(private readonly deps: GpuRunnerDeps) {}

  async run(input: Record<string, unknown>, sandbox: Sandbox): Promise<string> {
    const now = this.deps.now ?? Date.now;
    const jobId = String(input.job_id ?? '');
    if (!jobId) throw new WorkspaceError('job_id is required');
    const command = typeof input.command === 'string' ? input.command : '';
    if (!command.trim() || Buffer.byteLength(command) > MAX_COMMAND_BYTES)
      throw new WorkspaceError(
        `command must be a non-empty string up to ${MAX_COMMAND_BYTES} bytes`,
      );
    const filePaths = stringList(input.files, 'files', MAX_UPLOAD_FILES).map((path) =>
      safeRelativePath(path, 'each file'),
    );
    const outputs = stringList(input.outputs, 'outputs', MAX_OUTPUT_FILES).map((path) =>
      safeRelativePath(path, 'each output'),
    );
    const names = outputs.map((path) => basename(path));
    if (new Set(names).size !== names.length || names.some((name) => !ARTIFACT_NAME.test(name)))
      throw new WorkspaceError('outputs must have distinct file names');
    if (this.deps.recorded() + outputs.length > MAX_OUTPUT_FILES)
      throw new WorkspaceError(`a run can bring back at most ${MAX_OUTPUT_FILES} output files`);
    if (this.deps.commands().length >= MAX_GPU_COMMANDS)
      throw new WorkspaceError(`a run may execute at most ${MAX_GPU_COMMANDS} GPU commands`);

    const files: Array<{ path: string; bytes: Buffer }> = [];
    let uploadBytes = 0;
    for (const path of filePaths) {
      const bytes = sandbox.readBytes
        ? Buffer.from(await sandbox.readBytes(path))
        : Buffer.from(await sandbox.readFile(path), 'utf8');
      uploadBytes += bytes.length;
      if (uploadBytes > MAX_UPLOAD_BYTES)
        throw new WorkspaceError(`files may total at most ${MAX_UPLOAD_BYTES} bytes`);
      files.push({ path, bytes });
    }

    const commandOutcome: GpuCommandOutcome = {
      id: randomUUID(),
      job_id: jobId,
      status: 'pending',
      phase: 'waiting_for_compute',
      exit_code: null,
      timed_out: null,
      requested_outputs: outputs,
      collected_outputs: [],
      missing_outputs: [],
      oversized_outputs: [],
      failure: null,
      timings: {
        readinessWaitMs: null,
        workspaceReadyWaitMs: null,
        stagingMs: null,
        commandBudgetMs: null,
        commandElapsedMs: null,
        collectionMs: null,
      },
      stdout_tail: '',
      stderr_tail: '',
    };
    // Must be durable before any command can reach the provider. If persistence
    // fails, the in-memory pending outcome still prevents a false completed run.
    this.deps.recordCommand(commandOutcome);
    const readinessStarted = now();
    try {
      const ready = await this.deps.compute.awaitRunning(
        jobId,
        Math.min(
          now() + MAX_READY_WAIT_MS,
          this.deps.runDeadlineMs - RUN_MARGIN_MS - MIN_COMMAND_MS,
        ),
      );
      commandOutcome.timings.readinessWaitMs = Math.max(0, now() - readinessStarted);
      commandOutcome.phase = 'waiting_for_workspace';
      const requested = Number(input.timeout_secs);
      const deadlineMs = Math.min(
        ready.readySinceMs + ready.durationSecs * 1_000 - BOOKING_MARGIN_MS,
        this.deps.runDeadlineMs - RUN_MARGIN_MS,
        Number.isFinite(requested) && requested > 0 ? now() + requested * 1_000 : Infinity,
      );
      if (deadlineMs - now() < MIN_COMMAND_MS)
        throw new WorkspaceError('not enough booked GPU time is left to run a command');

      const client = (this.deps.connect ?? defaultConnect)(ready.accessUrl);
      let outcome: WorkspaceJobOutcome;
      try {
        outcome = await runWorkspaceJob(
          client,
          { command, files, outputs, deadlineMs },
          {
            signal: this.deps.signal,
            now,
            pollMs: this.deps.pollMs,
            onProgress: (progress) => {
              commandOutcome.phase = progress.phase;
              commandOutcome.timings = { ...commandOutcome.timings, ...progress.timings };
              this.deps.recordCommand(commandOutcome);
            },
          },
        );
      } finally {
        client.close();
      }

      // Keep provider observations even if local artifact persistence fails.
      commandOutcome.exit_code = outcome.exitCode;
      commandOutcome.timed_out = outcome.timedOut;
      commandOutcome.phase = 'complete';
      commandOutcome.timings = { ...commandOutcome.timings, ...outcome.timings };
      commandOutcome.stdout_tail = safeLogTail(outcome.stdoutTail, ready.accessUrl);
      commandOutcome.stderr_tail = safeLogTail(outcome.stderrTail, ready.accessUrl);
      const saved: GpuOutput[] = outcome.outputs.map(({ path, bytes }) => {
        const meta = this.deps.store.save(basename(path), bytes);
        return {
          job_id: jobId,
          path,
          bytes: meta.bytes,
          sha256: meta.sha256,
          url: this.deps.store.url(meta),
          expires_at: meta.expiresAt,
        };
      });
      if (saved.length) this.deps.record(saved);
      const collected = saved.map((output) => output.path);
      const skipped = outputs.filter((path) => outcome.skipped.includes(path));
      const missing = outputs.filter(
        (path) => !collected.includes(path) && !skipped.includes(path),
      );
      const failure: GpuFailure | null = outcome.timedOut
        ? 'timeout'
        : outcome.exitCode === null
          ? 'unknown_exit'
          : outcome.exitCode !== 0
            ? 'nonzero_exit'
            : skipped.length
              ? 'oversized_outputs'
              : missing.length
                ? 'missing_outputs'
                : null;
      const finished: GpuCommandOutcome = {
        ...commandOutcome,
        status: failure ? 'failed' : 'succeeded',
        phase: 'complete',
        exit_code: outcome.exitCode,
        timed_out: outcome.timedOut,
        collected_outputs: collected,
        missing_outputs: missing,
        oversized_outputs: skipped,
        failure,
        timings: { ...commandOutcome.timings, ...outcome.timings },
        stdout_tail: safeLogTail(outcome.stdoutTail, ready.accessUrl),
        stderr_tail: safeLogTail(outcome.stderrTail, ready.accessUrl),
      };
      this.deps.recordCommand(finished);
      const result = JSON.stringify({
        command_id: finished.id,
        status: finished.status,
        failure: finished.failure,
        job_id: jobId,
        exit_code: outcome.exitCode,
        timed_out: outcome.timedOut,
        duration_secs: Math.round(outcome.durationMs / 100) / 10,
        phase: finished.phase,
        timings: finished.timings,
        stdout_tail: finished.stdout_tail,
        stderr_tail: finished.stderr_tail,
        outputs: saved.map((output, index) => ({
          path: output.path,
          bytes: output.bytes,
          sha256: output.sha256,
          ...textPreview(outcome.outputs[index]!.bytes),
        })),
        missing,
        ...(skipped.length ? { over_size_limit: skipped } : {}),
      });
      if (failure) throw new GpuWorkloadError(result);
      return result;
    } catch (cause) {
      if (cause instanceof GpuWorkloadError) throw cause;
      if (commandOutcome.timings.readinessWaitMs === null)
        commandOutcome.timings.readinessWaitMs = Math.max(0, now() - readinessStarted);
      this.deps.recordCommand({ ...commandOutcome, status: 'failed', failure: 'execution_error' });
      // Remote transport errors may contain access URLs; retain fixed evidence only.
      throw new GpuWorkloadError(
        JSON.stringify({
          command_id: commandOutcome.id,
          status: 'failed',
          failure: 'execution_error',
          job_id: jobId,
          exit_code: commandOutcome.exit_code,
          timed_out: commandOutcome.timed_out,
          stdout_tail: commandOutcome.stdout_tail,
          stderr_tail: commandOutcome.stderr_tail,
          phase: commandOutcome.phase,
          timings: commandOutcome.timings,
          error: 'GPU execution or output collection could not be confirmed',
        }),
      );
    }
  }
}

function safeLogTail(text: string, accessUrl: string): string {
  let clean = text.replaceAll(accessUrl, '[workspace URL redacted]');
  const token = new URL(accessUrl).searchParams.get('token');
  if (token) clean = clean.replaceAll(token, '[workspace token redacted]');
  return clean.slice(-TAIL_CHARS);
}

function defaultConnect(accessUrl: string): WorkspaceClient {
  return new WorkspaceClient(parseWorkspaceUrl(accessUrl));
}

function stringList(value: unknown, label: string, max: number): unknown[] {
  if (value === undefined || value === null) return [];
  if (!Array.isArray(value) || value.length > max)
    throw new WorkspaceError(`${label} must be a list of at most ${max} paths`);
  return value;
}

/** Small UTF-8 outputs, such as a JSON report, are shown to the model in full. */
function textPreview(bytes: Buffer): { text?: string } {
  if (bytes.length > PREVIEW_BYTES) return {};
  const text = bytes.toString('utf8');
  return text.includes('�') || text.includes('\u0000') ? {} : { text };
}
