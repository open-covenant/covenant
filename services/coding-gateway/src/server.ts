import { createServer, type IncomingMessage, type ServerResponse } from "node:http";
import { randomUUID } from "node:crypto";
import type { GatewayEvent, RunState, RunStatus, SandboxProvider } from "./types.js";
import { selectBackend } from "./backends/index.js";
import { LocalSandboxProvider } from "./sandbox/local.js";
import { E2bSandboxProvider } from "./sandbox/e2b.js";
import { config } from "./config.js";
import { SpendLedger, modelCostUsd, sandboxCostUsd } from "./budget.js";

interface CapturedFile {
  path: string;
  content: string;
  truncated?: boolean;
}

interface Run {
  id: string;
  status: RunStatus;
  output?: string;
  error?: string;
  events: GatewayEvent[];
  subscribers: Set<ServerResponse>;
  abort: AbortController;
  files?: CapturedFile[];
}

// The sandbox is torn down when a run ends, so the only chance to show what
// got built is to read the workspace just before destroy. Cap aggressively —
// this is for a UI file tree, not a backup: skip dependency/build/VCS dirs and
// dotfiles, limit count + per-file + total bytes, and tolerate binaries.
const MAX_FILES = 40;
const MAX_FILE_BYTES = 64 * 1024;
const MAX_TOTAL_BYTES = 512 * 1024;

async function captureFiles(sandbox: {
  exec: (cmd: string, opts?: { timeoutMs?: number }) => Promise<{ stdout: string }>;
  readFile: (path: string) => Promise<string>;
}): Promise<CapturedFile[]> {
  const find =
    "find . -maxdepth 5 -type f " +
    "-not -path '*/node_modules/*' -not -path '*/.next/*' -not -path '*/.git/*' " +
    "-not -path '*/.npm/*' -not -path '*/.cache/*' -not -path '*/.config/*' " +
    "-not -path '*/dist/*' -not -name '.*' 2>/dev/null | head -120";
  const listed = await sandbox.exec(find, { timeoutMs: 15_000 }).catch(() => ({ stdout: "" }));
  const paths = listed.stdout
    .split("\n")
    .map((s) => s.trim().replace(/^\.\//, ""))
    .filter(Boolean);
  const files: CapturedFile[] = [];
  let total = 0;
  for (const path of paths) {
    if (files.length >= MAX_FILES || total >= MAX_TOTAL_BYTES) break;
    const raw = await sandbox.readFile(path).catch(() => null);
    if (raw == null) continue;
    // Skip apparent binaries (NUL byte) — readFile is UTF-8 only anyway.
    if (raw.includes("\u0000")) continue;
    const truncated = raw.length > MAX_FILE_BYTES;
    const content = truncated ? raw.slice(0, MAX_FILE_BYTES) : raw;
    files.push({ path, content, ...(truncated ? { truncated: true } : {}) });
    total += content.length;
  }
  files.sort((a, b) => a.path.localeCompare(b.path));
  return files;
}

const runs = new Map<string, Run>();
const ledger = new SpendLedger();

// E2B (ephemeral Firecracker microVM) when E2B_API_KEY is set, else the
// trusted-local provider for development. Egress allowlisting + custom resource
// caps still need a hardened E2B template — see src/sandbox/e2b.ts.
const provider: SandboxProvider = process.env.E2B_API_KEY
  ? new E2bSandboxProvider(process.env.E2B_API_KEY)
  : new LocalSandboxProvider();

const PORT = Number(process.env.PORT ?? process.env.GATEWAY_PORT ?? 8642);
const WALL_MS = Number(process.env.CODER_WALL_MS ?? 600_000);

// Serialize a GatewayEvent as the SSE frame covenantd's HermesRunner parses:
// it keys on `event` + `run_id` (not `type`) and reads `duration` in seconds
// for tool.completed. message/reasoning/file/run.* carry the same shape for the
// live UI (coder-08/09); the daemon ignores them for audit.
function sseFrame(e: GatewayEvent, runId: string): string {
  const r = runId;
  let w: Record<string, unknown>;
  switch (e.type) {
    case "tool.started":
      w = { event: "tool.started", run_id: r, tool: e.tool, preview: e.preview ?? "" };
      break;
    case "tool.completed":
      w = { event: "tool.completed", run_id: r, tool: e.tool, duration: e.duration_s ?? 0, error: e.error ?? false };
      break;
    case "approval.request":
      w = { event: "approval.request", run_id: r, choices: e.choices };
      break;
    case "file.written":
      w = { event: "file.written", run_id: r, path: e.path, bytes: e.bytes };
      break;
    case "message.delta":
      w = { event: "message.delta", run_id: r, text: e.text };
      break;
    case "reasoning.available":
      w = { event: "reasoning.available", run_id: r, text: e.text };
      break;
    case "run.completed":
      w = { event: "run.completed", run_id: r, output: e.output };
      break;
    case "run.failed":
      w = { event: "run.failed", run_id: r, error: e.error };
      break;
    default:
      w = { event: "unknown", run_id: r };
      break;
  }
  return `data: ${JSON.stringify(w)}\n\n`;
}

function publish(run: Run, e: GatewayEvent): void {
  run.events.push(e);
  const frame = sseFrame(e, run.id);
  for (const res of run.subscribers) res.write(frame);
}

function startRun(input: string, reservedMax: number): Run {
  const id = randomUUID();
  const run: Run = {
    id,
    status: "running",
    events: [],
    subscribers: new Set(),
    abort: new AbortController(),
  };
  runs.set(id, run);

  const wall = setTimeout(() => run.abort.abort(), WALL_MS);
  const startedAt = Date.now();
  const unsubscribeKill = ledger.onKill(() => run.abort.abort());

  void (async () => {
    let sandbox: Awaited<ReturnType<SandboxProvider["create"]>> | undefined;
    try {
      // provider.create() is INSIDE the try: an E2B / network failure here must
      // still release the reservation, free the concurrency slot, and
      // unsubscribe the kill handler — otherwise repeated provider failures
      // silently wedge the gateway at its caps with zero actual spend.
      //
      // Bracket the create() with abort checks so a kill that fires before the
      // microVM provisions skips it entirely (no spend), and a kill mid-create
      // tears the microVM down in `finally` without running a backend turn.
      if (run.abort.signal.aborted) throw new Error("aborted before sandbox create");
      sandbox = await provider.create({
        runId: id,
        egressAllowlist: ["registry.npmjs.org", "api.anthropic.com", "api.openai.com"],
        cpuMs: WALL_MS,
        memoryMb: 2048,
        diskMb: 5120,
        wallMs: WALL_MS,
      });
      if (run.abort.signal.aborted) throw new Error("aborted during sandbox create");
      const backend = selectBackend("anthropic");
      const { output, usage } = await backend.run({
        input,
        sandbox,
        signal: run.abort.signal,
        emit: (e) => publish(run, e),
      });
      run.output = output;
      // Snapshot the workspace BEFORE flipping status to completed, so a
      // client that polls and immediately fetches /files never races the
      // capture (status is the signal the run — and its artifacts — are ready).
      run.files = await captureFiles(sandbox).catch(() => []);
      run.status = "completed";
      const seconds = (Date.now() - startedAt) / 1000;
      ledger.commit(reservedMax, modelCostUsd(config.model, usage) + sandboxCostUsd(seconds), "completed");
    } catch (e) {
      run.error = (e as Error).message;
      // After abort (kill or stop) skip the file snapshot: it would exec
      // inside the still-alive sandbox for up to 15s, accruing spend the
      // operator just signalled they want to stop.
      run.files =
        sandbox && !run.abort.signal.aborted ? await captureFiles(sandbox).catch(() => []) : [];
      run.status = run.abort.signal.aborted ? "cancelled" : "failed";
      publish(run, { type: "run.failed", error: run.error });
      // No usage available on failure; charge the reserved max (pessimistic,
      // wallet-safe) since a partial run still spent tokens. Provider failures
      // before the sandbox exists charge $0.
      ledger.commit(
        reservedMax,
        sandbox ? reservedMax : 0,
        run.status === "cancelled" ? "cancelled" : "failed",
      );
    } finally {
      clearTimeout(wall);
      unsubscribeKill();
      if (sandbox) await sandbox.destroy().catch(() => {});
      for (const res of run.subscribers) res.end();
      run.subscribers.clear();
    }
  })();

  return run;
}

function json(res: ServerResponse, code: number, body: unknown): void {
  res.writeHead(code, { "content-type": "application/json" });
  res.end(JSON.stringify(body));
}

function streamEvents(run: Run, req: IncomingMessage, res: ServerResponse): void {
  res.writeHead(200, {
    "content-type": "text/event-stream",
    "cache-control": "no-cache",
    connection: "keep-alive",
    "x-accel-buffering": "no",
  });
  for (const e of run.events) res.write(sseFrame(e, run.id));
  if (run.status !== "running") {
    res.end();
    return;
  }
  run.subscribers.add(res);
  req.on("close", () => run.subscribers.delete(res));
}

function readBody(req: IncomingMessage): Promise<string> {
  return new Promise((resolve, reject) => {
    let data = "";
    req.on("data", (c) => (data += c));
    req.on("end", () => resolve(data));
    req.on("error", reject);
  });
}

export const server = createServer(async (req, res) => {
  try {
    const url = new URL(req.url ?? "/", "http://localhost");
    const parts = url.pathname.split("/").filter(Boolean);

    if (req.method === "GET" && url.pathname === "/v1/capabilities") {
      return json(res, 200, {
        features: {
          run_submission: true,
          run_events_sse: true,
          run_stop: true,
          run_approval_response: false,
        },
      });
    }

    if (req.method === "GET" && url.pathname === "/v1/budget") {
      return json(res, 200, ledger.snapshot());
    }

    if (req.method === "POST" && url.pathname === "/v1/runs") {
      const body = JSON.parse((await readBody(req)) || "{}") as { input?: unknown };
      if (typeof body.input !== "string" || !body.input.trim()) {
        return json(res, 400, { error: "input is required" });
      }
      const reservation = ledger.reserve();
      if (!reservation.ok) return json(res, 429, { error: reservation.reason });
      const run = startRun(body.input, reservation.max);
      return json(res, 200, { run_id: run.id });
    }

    if (parts[0] === "v1" && parts[1] === "runs" && parts[2]) {
      const run = runs.get(parts[2]);
      if (!run) return json(res, 404, { error: "run not found" });
      if (req.method === "GET" && parts.length === 3) {
        return json(res, 200, {
          status: run.status,
          output: run.output,
          error: run.error,
        } satisfies RunState);
      }
      if (req.method === "GET" && parts[3] === "files") {
        return json(res, 200, { files: run.files ?? [] });
      }
      if (req.method === "GET" && parts[3] === "events") return streamEvents(run, req, res);
      if (req.method === "POST" && parts[3] === "stop") {
        run.abort.abort();
        return json(res, 200, { status: "stopping" satisfies RunStatus });
      }
    }

    json(res, 404, { error: "not found" });
  } catch (e) {
    json(res, 500, { error: (e as Error).message });
  }
});

if (process.env.NODE_ENV !== "test") {
  // Operator kill-switch: `kill -USR1 <pid>` refuses new runs and aborts every
  // in-flight one. No HTTP surface, so no auth to get wrong. Idempotent.
  process.on("SIGUSR1", () => {
    console.warn(`SIGUSR1 received — engaging kill-switch (active=${ledger.snapshot().active})`);
    ledger.kill();
  });
  server.listen(PORT, () => {
    console.log(`coding-gateway listening on :${PORT} (model=${config.model}, effort=${config.effort})`);
  });
}
