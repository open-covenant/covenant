import { basename } from 'node:path';
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

    const ready = await this.deps.compute.awaitRunning(
      jobId,
      Math.min(now() + MAX_READY_WAIT_MS, this.deps.runDeadlineMs - RUN_MARGIN_MS - MIN_COMMAND_MS),
    );
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
        { signal: this.deps.signal, now, pollMs: this.deps.pollMs },
      );
    } finally {
      client.close();
    }

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

    return JSON.stringify({
      job_id: jobId,
      exit_code: outcome.exitCode,
      timed_out: outcome.timedOut,
      duration_secs: Math.round(outcome.durationMs / 100) / 10,
      stdout_tail: outcome.stdoutTail.slice(-TAIL_CHARS),
      stderr_tail: outcome.stderrTail.slice(-TAIL_CHARS),
      outputs: saved.map((output, index) => ({
        path: output.path,
        bytes: output.bytes,
        sha256: output.sha256,
        ...textPreview(outcome.outputs[index]!.bytes),
      })),
      missing: outcome.missing,
      ...(outcome.skipped.length ? { over_size_limit: outcome.skipped } : {}),
    });
  }
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
