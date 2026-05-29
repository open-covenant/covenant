/**
 * Contract for the Covenant coding gateway.
 *
 * The gateway implements the Hermes /v1 surface that covenantd's HermesRunner
 * already speaks, so the daemon runtime layer is unchanged. Per run it
 * provisions an ephemeral sandbox, drives a pluggable coding backend with
 * file + bash tools bound to that sandbox, and maps backend events onto the
 * Hermes SSE frames the daemon folds into its audit chain.
 *
 * Slices: coder-04 (gateway core + Anthropic backend), coder-05 (OpenAI
 * backend), coder-06/07 (SandboxProvider). No live model calls live here.
 */

// ── Hermes /v1 wire contract (consumed by covenantd HermesRunner) ──────────

export interface RunRequest {
  input: string;
  session_id?: string;
}

export interface RunCreated {
  run_id: string;
}

export type RunStatus =
  | "queued"
  | "running"
  | "waiting_for_approval"
  | "stopping"
  | "completed"
  | "failed"
  | "cancelled";

export interface RunState {
  status: RunStatus;
  output?: string;
  error?: string;
}

/** GET /v1/capabilities — the three features the daemon gates on plus the
 *  optional approval channel. */
export interface GatewayCapabilities {
  features: {
    run_submission: boolean;
    run_events_sse: boolean;
    run_stop: boolean;
    run_approval_response: boolean;
  };
}

// ── SSE events on GET /v1/runs/{id}/events ─────────────────────────────────
// covenantd maps tool.* and approval.* into RuntimeTrace audit rows;
// message/reasoning/file are for the live UI (WS3) and ignored by the tracer.

export type GatewayEvent =
  | { type: "tool.started"; tool: string; preview?: string }
  | { type: "tool.completed"; tool: string; duration_s?: number; error?: boolean }
  | { type: "file.written"; path: string; bytes: number }
  | { type: "message.delta"; text: string }
  | { type: "reasoning.available"; text: string }
  | { type: "approval.request"; choices: string[] }
  | { type: "run.completed"; output: string }
  | { type: "run.failed"; error: string };

// ── Pluggable coding backend (the "brain") ─────────────────────────────────
// Anthropic and OpenAI adapters implement this; selected per run.

export type BackendId = "anthropic" | "openai";

export interface TokenUsage {
  inputTokens: number;
  outputTokens: number;
  cacheReadTokens: number;
  cacheCreationTokens: number;
}

export interface CodingBackend {
  readonly id: BackendId;
  run(opts: {
    input: string;
    sandbox: Sandbox;
    signal: AbortSignal; // aborts on wall-clock budget or POST /stop
    emit: (e: GatewayEvent) => void;
  }): Promise<{ output: string; usage: TokenUsage }>;
}

// ── Pluggable execution substrate (the security boundary) ──────────────────
// E2B / Daytona / Modal adapter implements this.

export interface SandboxProvider {
  readonly id: string;
  create(spec: SandboxSpec): Promise<Sandbox>;
}

export interface SandboxSpec {
  runId: string;
  /** Hostnames the run may reach; everything else is blocked at the network
   *  layer. e.g. ["registry.npmjs.org", "api.anthropic.com"]. */
  egressAllowlist: string[];
  cpuMs: number;
  memoryMb: number;
  diskMb: number;
  wallMs: number;
}

export interface Sandbox {
  readFile(path: string): Promise<string>;
  writeFile(path: string, content: string): Promise<void>;
  exec(cmd: string, opts?: { timeoutMs?: number }): Promise<ExecResult>;
  /** Public URL serving a port inside the sandbox — used to preview a built
   *  web app in the UI (WS4). */
  previewUrl(port: number): Promise<string>;
  destroy(): Promise<void>;
}

export interface ExecResult {
  stdout: string;
  stderr: string;
  exitCode: number;
}
