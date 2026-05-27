import { createServer, type IncomingMessage, type ServerResponse } from "node:http";
import { randomUUID } from "node:crypto";
import type { GatewayEvent, RunState, RunStatus, SandboxProvider } from "./types.js";
import { selectBackend } from "./backends/index.js";
import { LocalSandboxProvider } from "./sandbox/local.js";
import { config } from "./config.js";
import { SpendLedger, modelCostUsd, sandboxCostUsd } from "./budget.js";

interface Run {
  id: string;
  status: RunStatus;
  output?: string;
  error?: string;
  events: GatewayEvent[];
  subscribers: Set<ServerResponse>;
  abort: AbortController;
}

const runs = new Map<string, Run>();
const ledger = new SpendLedger();

// Trusted-local provider for now; coder-07 swaps in the E2B provider so runs
// execute in an ephemeral, egress-capped sandbox before any public exposure.
const provider: SandboxProvider = new LocalSandboxProvider();

const PORT = Number(process.env.PORT ?? process.env.GATEWAY_PORT ?? 8642);
const WALL_MS = Number(process.env.CODER_WALL_MS ?? 600_000);

function publish(run: Run, e: GatewayEvent): void {
  run.events.push(e);
  const frame = `data: ${JSON.stringify(e)}\n\n`;
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

  void (async () => {
    const sandbox = await provider.create({
      runId: id,
      egressAllowlist: ["registry.npmjs.org", "api.anthropic.com", "api.openai.com"],
      cpuMs: WALL_MS,
      memoryMb: 2048,
      diskMb: 5120,
      wallMs: WALL_MS,
    });
    try {
      const backend = selectBackend("anthropic");
      const { output, usage } = await backend.run({
        input,
        sandbox,
        signal: run.abort.signal,
        emit: (e) => publish(run, e),
      });
      run.output = output;
      run.status = "completed";
      const seconds = (Date.now() - startedAt) / 1000;
      ledger.commit(reservedMax, modelCostUsd(config.model, usage) + sandboxCostUsd(seconds));
    } catch (e) {
      run.error = (e as Error).message;
      run.status = run.abort.signal.aborted ? "cancelled" : "failed";
      publish(run, { type: "run.failed", error: run.error });
      // No usage available on failure; charge the reserved max (pessimistic,
      // wallet-safe) since a partial run still spent tokens.
      ledger.commit(reservedMax, reservedMax);
    } finally {
      clearTimeout(wall);
      await sandbox.destroy().catch(() => {});
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
  for (const e of run.events) res.write(`data: ${JSON.stringify(e)}\n\n`);
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
  server.listen(PORT, () => {
    console.log(`coding-gateway listening on :${PORT} (model=${config.model}, effort=${config.effort})`);
  });
}
