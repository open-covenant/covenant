import { createServer, type IncomingMessage, type ServerResponse } from 'node:http';
import { createHash, randomUUID, timingSafeEqual } from 'node:crypto';
import type {
  GatewayEvent,
  RunArtifacts,
  RunRequest,
  RunState,
  RunStatus,
  ProviderReceipt,
  Sandbox,
  SandboxProvider,
  TokenUsage,
  ValidationResult,
} from './types.js';
import { selectBackend } from './backends/index.js';
import { LocalSandboxProvider } from './sandbox/local.js';
import { E2bSandboxProvider } from './sandbox/e2b.js';
import { config } from './config.js';
import { SpendLedger, modelCostUsd, sandboxCostUsd } from './budget.js';
import { IpBucket } from './ip-bucket.js';
import { sourceIp } from './source-ip.js';
import { admitRun } from './admit.js';
import { IdempotencyConflictError, RunStore, type StoredRun } from './run-store.js';
import { assertProductionConfig } from './config.js';
import { GatewayReadiness, verifyE2bTariff } from './readiness.js';
import {
  MAX_CHANGED_FILES,
  MAX_PATCH_BYTES,
  assertRepositoryFileCount,
  assertRepositoryPatchSize,
  captureRepositoryFiles,
} from './repository-artifacts.js';
import { isolatedShellCommand, quoteShellArgument } from './sandbox-command.js';
import { probeUsePodBalance, probeUsePodCatalog, type UsePodRequestConfig } from './usepod-http.js';

interface CapturedFile {
  path: string;
  content: string;
  truncated?: boolean;
}

interface Run extends StoredRun {
  subscribers: Set<ServerResponse>;
  abort: AbortController;
}

// The sandbox is torn down when a run ends, so the only chance to show what
// got built is to read the workspace just before destroy. Cap aggressively —
// this is for a UI file tree, not a backup: skip dependency/build/VCS dirs and
// dotfiles, limit count + per-file + total bytes, and tolerate binaries.
const MAX_FILES = 40;
const MAX_FILE_BYTES = 64 * 1024;
const MAX_TOTAL_BYTES = 512 * 1024;
const MAX_REQUEST_BODY_BYTES = 1_200_000;
const MAX_CHANGED_PATH_OUTPUT_BYTES = (MAX_CHANGED_FILES + 1) * 8_192;

async function captureFiles(sandbox: {
  exec: (
    cmd: string,
    opts?: { timeoutMs?: number },
  ) => Promise<{ stdout: string; exitCode?: number }>;
}): Promise<CapturedFile[]> {
  const find =
    'find . -maxdepth 5 -type f ' +
    "-not -path '*/node_modules/*' -not -path '*/.next/*' -not -path '*/.git/*' " +
    "-not -path '*/.npm/*' -not -path '*/.cache/*' -not -path '*/.config/*' " +
    "-not -path '*/dist/*' -not -name '.*' 2>/dev/null | head -120";
  const listed = await sandbox.exec(find, { timeoutMs: 15_000 }).catch(() => ({ stdout: '' }));
  const paths = listed.stdout
    .split('\n')
    .map((s) => s.trim().replace(/^\.\//, ''))
    .filter(Boolean);
  const files: CapturedFile[] = [];
  let total = 0;
  for (const path of paths) {
    if (files.length >= MAX_FILES || total >= MAX_TOTAL_BYTES) break;
    const captureLimit = Math.min(MAX_FILE_BYTES, MAX_TOTAL_BYTES - total);
    const result = await sandbox
      .exec(`head -c ${captureLimit + 1} -- ${quoteShellArgument(path)}`, {
        timeoutMs: 15_000,
      })
      .catch(() => null);
    if (!result || (result.exitCode !== undefined && result.exitCode !== 0)) continue;
    const raw = result.stdout;
    if (raw.includes('\u0000')) continue;
    const bytes = Buffer.from(raw, 'utf8');
    const truncated = bytes.length > captureLimit;
    const content = truncated ? bytes.subarray(0, captureLimit).toString('utf8') : raw;
    files.push({ path, content, ...(truncated ? { truncated: true } : {}) });
    total += Buffer.byteLength(content, 'utf8');
  }
  files.sort((a, b) => a.path.localeCompare(b.path));
  return files;
}

assertProductionConfig();
const runStore = new RunStore();
const runs = new Map<string, Run>(
  runStore
    .list()
    .map((stored) => [
      stored.id,
      { ...stored, subscribers: new Set<ServerResponse>(), abort: new AbortController() },
    ]),
);
const ledger = new SpendLedger();
const ipBucket = new IpBucket({
  maxPerIp: config.ipMaxPerIp,
  refillMs: config.ipRefillMs,
});

// E2B (ephemeral Firecracker microVM) when E2B_API_KEY is set, else the
// trusted-local provider for development. Production boot requires the E2B
// provider, pinned outbound hosts, and persistent run receipts.
const e2bIdentity = config.e2bTemplateId
  ? {
      templateId: config.e2bTemplateId,
      cpuCount: config.e2bExpectedCpuCount,
      memoryMb: config.e2bExpectedMemoryMb,
    }
  : undefined;
const provider: SandboxProvider = process.env.E2B_API_KEY
  ? new E2bSandboxProvider(process.env.E2B_API_KEY, e2bIdentity, config.e2bEgressAllowlist)
  : new LocalSandboxProvider();

const PORT = Number(process.env.PORT ?? process.env.GATEWAY_PORT ?? 8642);
const modelProbe = modelProbeConfig();
const readiness = new GatewayReadiness({
  provider,
  model: modelProbe,
  balance: balanceProbeConfig(),
  tariff: e2bIdentity
    ? {
        check: () =>
          verifyE2bTariff({
            reference: config.sandboxTariffRef,
            ...e2bIdentity,
            worstCaseUsdPerSec: config.sandboxWorstCaseUsdPerSec,
          }),
      }
    : undefined,
  refreshMs: config.readinessRefreshMs,
  maxAgeMs: config.readinessMaxAgeMs,
  timeoutMs: config.readinessTimeoutMs,
});

// Serialize a GatewayEvent as the SSE frame covenantd's HermesRunner parses:
// it keys on `event` + `run_id` (not `type`) and reads `duration` in seconds
// for tool.completed. message/reasoning/file/run.* carry the same shape for the
// live UI; the daemon ignores them for audit.
function sseFrame(e: GatewayEvent, runId: string): string {
  const r = runId;
  let w: Record<string, unknown>;
  switch (e.type) {
    case 'tool.started':
      w = { event: 'tool.started', run_id: r, tool: e.tool, preview: e.preview ?? '' };
      break;
    case 'tool.completed':
      w = {
        event: 'tool.completed',
        run_id: r,
        tool: e.tool,
        duration: e.duration_s ?? 0,
        error: e.error ?? false,
      };
      break;
    case 'approval.request':
      w = { event: 'approval.request', run_id: r, choices: e.choices };
      break;
    case 'file.written':
      w = { event: 'file.written', run_id: r, path: e.path, bytes: e.bytes };
      break;
    case 'message.delta':
      w = { event: 'message.delta', run_id: r, text: e.text };
      break;
    case 'reasoning.available':
      w = { event: 'reasoning.available', run_id: r, text: e.text };
      break;
    case 'run.completed':
      w = { event: 'run.completed', run_id: r, output: e.output };
      break;
    case 'run.failed':
      w = { event: 'run.failed', run_id: r, error: e.error };
      break;
    default:
      w = { event: 'unknown', run_id: r };
      break;
  }
  return `data: ${JSON.stringify(w)}\n\n`;
}

const MAX_RETAINED_EVENTS = 256;
const MAX_EVENT_TEXT = 8_000;

function publish(run: Run, event: GatewayEvent): void {
  const e = boundedEvent(event);
  run.events.push(e);
  if (run.events.length > MAX_RETAINED_EVENTS) {
    run.events.splice(0, run.events.length - MAX_RETAINED_EVENTS);
  }
  run.updatedAt = new Date().toISOString();
  const frame = sseFrame(e, run.id);
  for (const res of run.subscribers) res.write(frame);
}

function boundedEvent(event: GatewayEvent): GatewayEvent {
  switch (event.type) {
    case 'message.delta':
    case 'reasoning.available':
      return { ...event, text: event.text.slice(0, MAX_EVENT_TEXT) };
    case 'run.completed':
      return { ...event, output: event.output.slice(0, MAX_EVENT_TEXT) };
    case 'run.failed':
      return { ...event, error: event.error.slice(0, MAX_EVENT_TEXT) };
    case 'tool.started':
      return { ...event, preview: event.preview?.slice(0, 500) };
    case 'approval.request':
      return {
        ...event,
        choices: event.choices.slice(0, 20).map((choice) => choice.slice(0, 500)),
      };
    default:
      return event;
  }
}

function startRun(
  id: string,
  request: RunRequest,
  requestFingerprint: string,
  reservedMax: number,
  reservationId: string,
  sourceIpStr: string,
  exempt: boolean,
): Run {
  const run: Run = {
    id,
    sessionId: request.session_id,
    requestFingerprint,
    reservationId,
    reservedMax,
    status: 'running',
    events: [],
    subscribers: new Set(),
    abort: new AbortController(),
    updatedAt: new Date().toISOString(),
  };
  runs.set(id, run);
  try {
    persistRun(run);
  } catch (cause) {
    runs.delete(id);
    ledger.commit(reservationId, reservedMax, 0, 'failed');
    if (!exempt) ipBucket.release(sourceIpStr);
    throw cause;
  }

  const wall = setTimeout(() => run.abort.abort(), config.wallMs);
  const startedAt = Date.now();
  const unsubscribeKill = ledger.onKill(() => run.abort.abort());
  console.log(
    `run ${id} started (input=${request.input.length}b, provider=${provider.id}, model=${config.model})`,
  );

  void (async () => {
    let sandbox: Awaited<ReturnType<SandboxProvider['create']>> | undefined;
    let sandboxCreateAttempted = false;
    try {
      // provider.create() is INSIDE the try: an E2B / network failure here must
      // still release the reservation, free the concurrency slot, and
      // unsubscribe the kill handler — otherwise repeated provider failures
      // silently wedge the gateway at its caps with zero actual spend.
      //
      // Bracket the create() with abort checks so a kill that fires before the
      // microVM provisions skips it entirely (no spend), and a kill mid-create
      // tears the microVM down in `finally` without running a backend turn.
      if (run.abort.signal.aborted) throw new Error('aborted before sandbox create');
      sandboxCreateAttempted = true;
      sandbox = await provider.create({
        runId: id,
        egressAllowlist: [...config.e2bEgressAllowlist],
        cpuMs: config.wallMs,
        memoryMb: 2048,
        diskMb: 5120,
        wallMs: config.wallMs,
      });
      if (run.abort.signal.aborted) throw new Error('aborted during sandbox create');
      if (request.repository_url) {
        await initializeRepository(sandbox, request.repository_url, request.base_sha);
        if (request.initial_patch) await applyInitialPatch(sandbox, request.initial_patch);
      }
      console.log(`run ${id} sandbox ready (${Date.now() - startedAt}ms) -> backend start`);
      const backend = selectBackend(config.backend);
      const { output, usage, providerReceipts } = await backend.run({
        input: request.input,
        sandbox,
        signal: run.abort.signal,
        emit: (e) => publish(run, e),
        maxProviderCostUsd: providerBudgetUsd(reservedMax),
        recordProviderRequest: () => {
          const previous = run.providerRequestCount ?? 0;
          run.providerRequestCount = previous + 1;
          run.updatedAt = new Date().toISOString();
          try {
            persistRun(run);
          } catch (cause) {
            run.providerRequestCount = previous;
            throw cause;
          }
        },
        recordProviderReceipt: (receipt) => {
          const previous = run.providerReceipts;
          run.providerReceipts = [...(previous ?? []), receipt];
          run.updatedAt = new Date().toISOString();
          try {
            persistRun(run);
          } catch (cause) {
            run.providerReceipts = previous;
            throw cause;
          }
        },
      });
      run.output = output;
      run.usage = usage;
      run.providerReceipts = providerReceipts ?? run.providerReceipts;
      if (request.repository_url) {
        const artifacts = await collectRepositoryArtifacts(
          sandbox,
          request.validation_commands ?? [],
        );
        run.patch = artifacts.patch;
        run.changedFiles = artifacts.changedFiles;
        run.validations = artifacts.validations;
        if (artifacts.validations.some((result) => result.exitCode !== 0)) {
          throw new Error('one or more declared validation commands failed');
        }
      }
      // Snapshot the workspace BEFORE flipping status to completed, so a
      // client that polls and immediately fetches /files never races the
      // capture (status is the signal the run — and its artifacts — are ready).
      run.files = request.repository_url
        ? await captureRepositoryFiles(sandbox, run.changedFiles ?? [])
        : await captureFiles(sandbox).catch(() => []);
      run.status = 'completed';
      run.updatedAt = new Date().toISOString();
      const seconds = (Date.now() - startedAt) / 1000;
      run.costUsd = completedRunCost(
        run.providerRequestCount ?? 0,
        providerReceipts ?? run.providerReceipts,
        usage,
        sandboxAccountingChargeUsd(sandboxCreateAttempted),
        reservedMax,
      );
      console.log(
        `run ${id} completed (${Math.round(seconds * 1000)}ms, ${run.files?.length ?? 0} files, ${run.events.length} events)`,
      );
      persistRun(run);
      ledger.commit(reservationId, reservedMax, run.costUsd, 'completed');
    } catch (e) {
      run.error = (e as Error).message;
      console.error(
        `run ${id} ${run.abort.signal.aborted ? 'cancelled' : 'FAILED'} (${Date.now() - startedAt}ms): ${run.error}`,
      );
      run.status = run.abort.signal.aborted ? 'cancelled' : 'failed';
      run.costUsd = failedRunCost(
        run.providerRequestCount ?? 0,
        run.providerReceipts,
        sandboxAccountingChargeUsd(sandboxCreateAttempted),
        reservedMax,
      );
      if (run.costUsd > reservedMax + 1e-9) ledger.kill();
      // After abort (kill or stop) skip the file snapshot: it would exec
      // inside the still-alive sandbox for up to 15s, accruing spend the
      // operator just signalled they want to stop.
      run.files =
        sandbox && !run.abort.signal.aborted
          ? request.repository_url
            ? await captureRepositoryFiles(sandbox, run.changedFiles ?? []).catch(() => [])
            : await captureFiles(sandbox).catch(() => [])
          : [];
      publish(run, { type: 'run.failed', error: run.error });
      ledger.commit(
        reservationId,
        reservedMax,
        run.costUsd,
        run.status === 'cancelled' ? 'cancelled' : 'failed',
      );
      run.updatedAt = new Date().toISOString();
      try {
        persistRun(run);
      } catch (persistError) {
        console.error(
          `run ${id} terminal receipt persistence failed: ${(persistError as Error).message}`,
        );
      }
    } finally {
      clearTimeout(wall);
      unsubscribeKill();
      // Release the per-IP slot on every terminal outcome (success,
      // failure, cancel). A leak here would pin the bucket against a
      // legit client until the next service restart. Exempt IPs never
      // took a slot, so skip — release would be a harmless no-op against
      // the slot map, but skipping keeps the intent explicit.
      if (!exempt) ipBucket.release(sourceIpStr);
      if (sandbox) await sandbox.destroy().catch(() => {});
      for (const res of run.subscribers) res.end();
      run.subscribers.clear();
    }
  })();

  return run;
}

function json(res: ServerResponse, code: number, body: unknown): void {
  res.writeHead(code, {
    'content-type': 'application/json',
    'cache-control': 'private, no-store',
    'x-content-type-options': 'nosniff',
  });
  res.end(JSON.stringify(body));
}

function persistRun(run: Run): void {
  const { subscribers: _subscribers, abort: _abort, ...stored } = run;
  runStore.save(stored);
}

function modelProbeConfig(): { expectedModel: string; check: () => Promise<void> } {
  if (config.backend === 'usepod') {
    const probe = usePodProbeConfig();
    return {
      expectedModel: config.model,
      check: () => probeUsePodCatalog(probe),
    };
  }
  if (config.backend === 'openai') {
    const key = process.env.OPENAI_API_KEY ?? '';
    return {
      expectedModel: config.model,
      check: () =>
        probeModelCatalog(
          'https://api.openai.com/v1/models',
          { authorization: `Bearer ${key}` },
          config.model,
        ),
    };
  }
  const key = process.env.ANTHROPIC_API_KEY ?? '';
  return {
    expectedModel: config.model,
    check: () =>
      probeModelCatalog(
        'https://api.anthropic.com/v1/models',
        { 'x-api-key': key, 'anthropic-version': '2023-06-01' },
        config.model,
      ),
  };
}

function balanceProbeConfig(): { check: () => Promise<void> } | undefined {
  if (config.backend !== 'usepod') return undefined;
  const probe = usePodProbeConfig();
  return { check: () => probeUsePodBalance(probe) };
}

function usePodProbeConfig(): UsePodRequestConfig {
  return {
    baseUrl: config.usePodBaseUrl,
    token: process.env.USEPOD_API_KEY ?? '',
    model: config.model,
    maxInputPriceMicrounits: config.usePodMaxInputPriceMicrounits,
    maxOutputPriceMicrounits: config.usePodMaxOutputPriceMicrounits,
    minimumBalance: config.usePodMinimumBalance,
  };
}

async function probeModelCatalog(
  url: string,
  headers: Record<string, string>,
  expectedModel: string,
): Promise<void> {
  const response = await fetch(url, { headers, signal: AbortSignal.timeout(15_000) });
  if (!response.ok) throw new Error(`model readiness failed with HTTP ${response.status}`);
  const body = (await response.json()) as { data?: Array<{ id?: unknown }> };
  if (!body.data?.some((entry) => entry.id === expectedModel)) {
    throw new Error('configured model is absent from the provider catalog');
  }
}

function providerAccountedCostUsd(
  requestCount: number,
  receipts: ProviderReceipt[] | undefined,
): number | undefined {
  if (requestCount === 0) return receipts?.length ? undefined : 0;
  if (
    !receipts ||
    receipts.length !== requestCount ||
    receipts.some(
      (receipt) =>
        !receipt.accounting ||
        receipt.model !== config.model ||
        receipt.accounting.inputPriceMicrounitsPerMillion !==
          config.usePodMaxInputPriceMicrounits ||
        receipt.accounting.outputPriceMicrounitsPerMillion !==
          config.usePodMaxOutputPriceMicrounits,
    )
  ) {
    return undefined;
  }
  let total: bigint;
  try {
    total = receipts.reduce(
      (sum, receipt) => sum + BigInt(receipt.accounting!.accountedCostMicrounits),
      0n,
    );
  } catch {
    return undefined;
  }
  if (total > BigInt(Number.MAX_SAFE_INTEGER)) {
    console.error('provider receipt total exceeds the exact accounting range');
    return undefined;
  }
  return Number(total) / 1_000_000;
}

function completedRunCost(
  providerRequestCount: number,
  receipts: ProviderReceipt[] | undefined,
  usage: TokenUsage,
  sandboxUsd: number,
  reservedMax: number,
): number {
  const modelUsd =
    config.backend === 'usepod'
      ? providerAccountedCostUsd(providerRequestCount, receipts)
      : modelCostUsd(config.model, usage);
  if (modelUsd === undefined) return reservedMax;
  const total = modelUsd + sandboxUsd;
  if (!Number.isFinite(total) || total < 0) return reservedMax;
  return total;
}

export function failedRunCost(
  providerRequestCount: number,
  receipts: ProviderReceipt[] | undefined,
  sandboxUsd: number,
  reservedMax: number,
): number {
  const providerUsd = providerAccountedCostUsd(providerRequestCount, receipts);
  if (providerUsd === undefined) return reservedMax;
  const total = providerUsd + sandboxUsd;
  return Number.isFinite(total) && total >= 0 ? total : reservedMax;
}

export function maximumSandboxCostUsd(): number {
  return sandboxCostUsd(config.wallMs / 1_000);
}

export function providerBudgetUsd(reservedMax: number): number {
  const budget = reservedMax - maximumSandboxCostUsd();
  if (!Number.isFinite(budget) || budget <= 0) {
    throw new Error('reservation cannot fund the maximum sandbox charge');
  }
  return budget;
}

export function sandboxAccountingChargeUsd(createAttempted: boolean): number {
  return createAttempted ? maximumSandboxCostUsd() : 0;
}

function streamEvents(run: Run, req: IncomingMessage, res: ServerResponse): void {
  res.writeHead(200, {
    'content-type': 'text/event-stream',
    'cache-control': 'no-cache',
    connection: 'keep-alive',
    'x-accel-buffering': 'no',
  });
  for (const e of run.events) res.write(sseFrame(e, run.id));
  if (run.status !== 'running') {
    res.end();
    return;
  }
  run.subscribers.add(res);
  req.on('close', () => run.subscribers.delete(res));
}

export function readBody(req: IncomingMessage, maxBytes = MAX_REQUEST_BODY_BYTES): Promise<string> {
  return new Promise((resolve, reject) => {
    const chunks: Buffer[] = [];
    let size = 0;
    let settled = false;
    const fail = (cause: Error) => {
      if (settled) return;
      settled = true;
      req.removeListener('data', onData);
      req.removeListener('end', onEnd);
      req.resume();
      reject(cause);
    };
    const onData = (chunk: Buffer | string) => {
      const buffer = Buffer.isBuffer(chunk) ? chunk : Buffer.from(chunk);
      size += buffer.byteLength;
      if (size > maxBytes) {
        fail(new RequestBodyTooLargeError());
        return;
      }
      chunks.push(buffer);
    };
    const onEnd = () => {
      if (settled) return;
      settled = true;
      resolve(Buffer.concat(chunks).toString('utf8'));
    };
    const length = Number(req.headers['content-length']);
    if (Number.isFinite(length) && length > maxBytes) {
      fail(new RequestBodyTooLargeError());
      return;
    }
    req.on('data', onData);
    req.on('end', onEnd);
    req.on('error', fail);
  });
}

type ParsedRun = { ok: true; value: RunRequest } | { ok: false; error: string };

export function parseRunRequest(body: Partial<RunRequest>): ParsedRun {
  if (
    typeof body.session_id !== 'string' ||
    !/^[A-Za-z0-9][A-Za-z0-9._:-]{0,127}$/.test(body.session_id)
  ) {
    return { ok: false, error: 'session_id must be a valid 1-128 character idempotency key' };
  }
  if (typeof body.input !== 'string' || !body.input.trim()) {
    return { ok: false, error: 'input is required' };
  }
  if (body.input.length > 64_000) return { ok: false, error: 'input exceeds 64KB' };
  if (typeof body.max_cost_usd !== 'number' || !Number.isFinite(body.max_cost_usd)) {
    return { ok: false, error: 'max_cost_usd must be a finite number' };
  }
  if (body.max_cost_usd <= 0 || body.max_cost_usd > config.perRunUsdMax) {
    return {
      ok: false,
      error: `max_cost_usd must be greater than zero and no more than ${config.perRunUsdMax}`,
    };
  }
  if (body.max_cost_usd <= maximumSandboxCostUsd()) {
    return { ok: false, error: 'max_cost_usd cannot fund the maximum sandbox charge' };
  }

  let repositoryUrl: string | undefined;
  if (body.repository_url !== undefined) {
    if (typeof body.repository_url !== 'string') {
      return { ok: false, error: 'repository_url must be a string' };
    }
    const match = body.repository_url.match(
      /^https:\/\/github\.com\/([A-Za-z0-9_.-]+)\/([A-Za-z0-9_.-]+?)(?:\.git)?$/,
    );
    if (!match) return { ok: false, error: 'only public GitHub HTTPS repositories are supported' };
    repositoryUrl = `https://github.com/${match[1]}/${match[2]}.git`;
  }

  if (body.base_sha !== undefined && !/^[a-f0-9]{40}$/i.test(body.base_sha)) {
    return { ok: false, error: 'base_sha must be a 40-character Git SHA' };
  }
  if (body.base_sha && !repositoryUrl) {
    return { ok: false, error: 'base_sha requires repository_url' };
  }

  const validations = body.validation_commands ?? [];
  if (!Array.isArray(validations) || validations.length > 3) {
    return { ok: false, error: 'validation_commands must contain at most three commands' };
  }
  if (
    validations.some(
      (command) => typeof command !== 'string' || !command.trim() || command.length > 500,
    )
  ) {
    return { ok: false, error: 'invalid validation command' };
  }
  if (body.initial_patch !== undefined) {
    if (!repositoryUrl) return { ok: false, error: 'initial_patch requires repository_url' };
    if (typeof body.initial_patch !== 'string' || body.initial_patch.length > 1_000_000) {
      return { ok: false, error: 'initial_patch must be a string no larger than 1MB' };
    }
  }

  return {
    ok: true,
    value: {
      input: body.input,
      session_id: body.session_id,
      max_cost_usd: body.max_cost_usd,
      repository_url: repositoryUrl,
      base_sha: body.base_sha,
      validation_commands: validations,
      initial_patch: body.initial_patch,
    },
  };
}

export function runRequestFingerprint(request: RunRequest): string {
  return createHash('sha256')
    .update(
      JSON.stringify({
        input: request.input,
        max_cost_usd: request.max_cost_usd,
        repository_url: request.repository_url,
        base_sha: request.base_sha,
        validation_commands: request.validation_commands ?? [],
        initial_patch: request.initial_patch,
      }),
    )
    .digest('hex');
}

async function applyInitialPatch(sandbox: Sandbox, patch: string): Promise<void> {
  const path = '.mizuki-initial.patch';
  await sandbox.writeFile(path, patch);
  const result = await sandbox.exec(`git apply --whitespace=nowarn '${path}' && rm '${path}'`, {
    timeoutMs: 30_000,
  });
  if (result.exitCode !== 0) {
    throw new Error(`initial patch could not be applied: ${result.stderr.slice(0, 1_000)}`);
  }
}

async function initializeRepository(
  sandbox: Sandbox,
  repositoryUrl: string,
  baseSha?: string,
): Promise<void> {
  const command = baseSha
    ? `git init -q && git remote add origin '${repositoryUrl}' && git fetch -q --depth=1 origin '${baseSha}' && git checkout -q --detach FETCH_HEAD`
    : `git clone -q --depth=1 '${repositoryUrl}' .`;
  const result = await sandbox.exec(command, { timeoutMs: 120_000 });
  if (result.exitCode !== 0) {
    throw new Error(`repository checkout failed: ${result.stderr.slice(0, 1_000)}`);
  }
  await sandbox.exec(
    'git config user.name Mizuki && git config user.email mizuki@users.noreply.github.com',
  );
}

async function collectRepositoryArtifacts(
  sandbox: Sandbox,
  commands: string[],
): Promise<Pick<RunArtifacts, 'patch' | 'changedFiles' | 'validations'>> {
  assertRepositoryFileCount(await listRepositoryChanges(sandbox));
  const validations: ValidationResult[] = [];
  for (const command of commands) {
    const result = await sandbox.exec(isolatedShellCommand(command), { timeoutMs: 300_000 });
    validations.push({
      command,
      exitCode: result.exitCode,
      stdout: result.stdout.slice(0, 64_000),
      stderr: result.stderr.slice(0, 64_000),
    });
  }

  assertRepositoryFileCount(await listRepositoryChanges(sandbox));
  const intent = await sandbox.exec('git add -N .', { timeoutMs: 30_000 });
  if (intent.exitCode !== 0) throw new Error('failed to prepare repository diff');

  const diff = await sandbox.exec(
    boundedGitOutput('git diff --binary --no-ext-diff', MAX_PATCH_BYTES + 1),
    { timeoutMs: 30_000 },
  );
  if (diff.exitCode !== 0) throw new Error('failed to collect repository diff');
  assertRepositoryPatchSize(diff.stdout);

  const names = await sandbox.exec(
    boundedGitOutput('git diff --name-only -z --no-ext-diff', MAX_CHANGED_PATH_OUTPUT_BYTES),
    { timeoutMs: 30_000 },
  );
  if (names.exitCode !== 0) throw new Error('failed to collect changed file names');
  const changedFiles = names.stdout.split('\0').filter(Boolean);
  assertRepositoryFileCount(changedFiles);
  return {
    patch: diff.stdout,
    changedFiles,
    validations,
  };
}

async function listRepositoryChanges(sandbox: Sandbox): Promise<string[]> {
  const result = await sandbox.exec(
    boundedGitOutput(
      'git ls-files --modified --deleted --others --exclude-standard -z',
      MAX_CHANGED_PATH_OUTPUT_BYTES,
    ),
    { timeoutMs: 30_000 },
  );
  if (result.exitCode !== 0) throw new Error('failed to enumerate repository changes');
  return [...new Set(result.stdout.split('\0').filter(Boolean))];
}

function boundedGitOutput(command: string, maxBytes: number): string {
  const script = [
    `${command} | head -c ${maxBytes}`,
    'statuses=("${PIPESTATUS[@]}")',
    '[ "${statuses[1]}" -eq 0 ] || exit "${statuses[1]}"',
    '[ "${statuses[0]}" -eq 0 ] || [ "${statuses[0]}" -eq 141 ]',
  ].join('\n');
  return `/bin/bash -c ${quoteShellArgument(script)}`;
}

export const server = createServer(async (req, res) => {
  try {
    const url = new URL(req.url ?? '/', 'http://localhost');
    const parts = url.pathname.split('/').filter(Boolean);

    if (req.method === 'GET' && url.pathname === '/healthz') {
      return json(res, 200, {
        ok: true,
        backend: config.backend,
        provider: provider.id,
        persistentRuns: runStore.persistent,
        storageReady: runStore.persistenceReady && ledger.snapshot().persistenceReady,
      });
    }

    if (req.method === 'GET' && url.pathname === '/readyz') {
      if (!authorized(req, config.authToken)) return json(res, 401, { error: 'unauthorized' });
      const report = await readiness.check();
      const storage = {
        ledger: ledger.snapshot().persistenceReady,
        runStore: runStore.persistenceReady,
      };
      const storageFailed = Object.entries(storage)
        .filter(([, ready]) => !ready)
        .map(([name]) => name);
      const ready = report.ready && storageFailed.length === 0;
      return json(res, ready ? 200 : 503, {
        ...report,
        ready,
        failed: [...report.failed, ...storageFailed],
        backend: config.backend,
        provider: provider.id,
        persistentRuns: runStore.persistent,
        storage,
      });
    }

    if (url.pathname.startsWith('/v1/') && !authorized(req, config.authToken)) {
      return json(res, 401, { error: 'unauthorized' });
    }

    if (req.method === 'GET' && url.pathname === '/v1/capabilities') {
      return json(res, 200, {
        features: {
          run_submission: true,
          run_events_sse: true,
          run_stop: true,
          run_approval_response: false,
        },
      });
    }

    if (req.method === 'GET' && url.pathname === '/v1/budget') {
      return json(res, 200, {
        ...ledger.snapshot(),
        accounting: {
          sandbox: {
            method: 'pinned-worst-case-tariff',
            usdPerSec: config.sandboxWorstCaseUsdPerSec,
            tariffRef: config.sandboxTariffRef,
            maximumPerRunUsd: maximumSandboxCostUsd(),
            authoritativeBillingReceipt: false,
            identity: e2bIdentity ?? null,
          },
        },
        ipBucket: ipBucket.snapshot(),
      });
    }

    if (req.method === 'POST' && url.pathname === '/v1/runs') {
      const readinessReport = await readiness.check();
      if (!readinessReport.ready) {
        return json(res, 503, {
          error: 'gateway dependencies are not ready',
          failed: readinessReport.failed,
        });
      }
      const ledgerState = ledger.snapshot();
      if (!runStore.persistenceReady || !ledgerState.persistenceReady) {
        return json(res, 503, { error: 'gateway persistence is unavailable' });
      }
      const body = JSON.parse((await readBody(req)) || '{}') as Partial<RunRequest>;
      const parsed = parseRunRequest(body);
      if (!parsed.ok) {
        return json(res, 400, { error: parsed.error });
      }
      if (
        parsed.value.repository_url &&
        provider.id === 'local' &&
        process.env.ALLOW_LOCAL_REPOSITORY_RUNS !== '1'
      ) {
        return json(res, 503, {
          error:
            'repository runs require an isolated sandbox; set ALLOW_LOCAL_REPOSITORY_RUNS=1 only for trusted local development',
        });
      }
      const fingerprint = runRequestFingerprint(parsed.value);
      const replay = runStore.replay(parsed.value.session_id, fingerprint);
      if (replay) return json(res, 200, { run_id: replay.id });
      if (!parsed.value.input.trim()) {
        return json(res, 400, { error: 'input is required' });
      }
      const ip = sourceIp(req, config.trustedProxyHops);
      const runId = randomUUID();
      const outcome = admitRun({
        runId,
        maxUsd: parsed.value.max_cost_usd,
        ip,
        ipBucket,
        ledger,
        ipMaxPerIp: config.ipMaxPerIp,
        exemptIps: config.exemptIps,
      });
      if (!outcome.ok) {
        if (outcome.retryAfterMs !== undefined) {
          res.setHeader('retry-after', Math.ceil(outcome.retryAfterMs / 1000));
        }
        return json(res, 429, { error: outcome.reason });
      }
      const run = startRun(
        runId,
        parsed.value,
        fingerprint,
        outcome.reservedMax,
        outcome.reservationId,
        ip,
        outcome.exempt,
      );
      return json(res, 200, { run_id: run.id });
    }

    if (parts[0] === 'v1' && parts[1] === 'runs' && parts[2]) {
      const run = runs.get(parts[2]);
      if (!run) return json(res, 404, { error: 'run not found' });
      if (req.method === 'GET' && parts.length === 3) {
        return json(res, 200, {
          status: run.status,
          output: run.output,
          error: run.error,
          usage: run.usage,
          costUsd: run.costUsd,
          providerReceipts: run.providerReceipts,
        } satisfies RunState);
      }
      if (req.method === 'GET' && parts[3] === 'artifacts') {
        const files = (run.files ?? [])
          .filter((file) => run.changedFiles?.includes(file.path))
          .map(({ path, content }) => ({ path, content }));
        return json(res, 200, {
          patch: run.patch ?? '',
          changedFiles: run.changedFiles ?? [],
          files,
          validations: run.validations ?? [],
        } satisfies RunArtifacts);
      }
      if (req.method === 'GET' && parts[3] === 'files') {
        return json(res, 200, { files: run.files ?? [] });
      }
      if (req.method === 'GET' && parts[3] === 'events') return streamEvents(run, req, res);
      if (req.method === 'POST' && parts[3] === 'stop') {
        run.abort.abort();
        return json(res, 200, { status: 'stopping' satisfies RunStatus });
      }
    }

    json(res, 404, { error: 'not found' });
  } catch (e) {
    const status =
      e instanceof RequestBodyTooLargeError
        ? 413
        : e instanceof IdempotencyConflictError
          ? 409
          : e instanceof SyntaxError
            ? 400
            : 500;
    json(res, status, { error: (e as Error).message });
  }
});

class RequestBodyTooLargeError extends Error {
  constructor() {
    super('request body exceeds the 1.2MB limit');
  }
}

function authorized(req: IncomingMessage, expected: string | undefined): boolean {
  if (!expected) return true;
  const supplied = req.headers.authorization?.replace(/^Bearer\s+/i, '');
  if (!supplied) return false;
  const left = Buffer.from(supplied);
  const right = Buffer.from(expected);
  return left.length === right.length && timingSafeEqual(left, right);
}

if (process.env.NODE_ENV !== 'test') {
  // Operator kill-switch: `kill -USR1 <pid>` refuses new runs and aborts every
  // in-flight one. No HTTP surface, so no auth to get wrong. Idempotent.
  process.on('SIGUSR1', () => {
    console.warn(`SIGUSR1 received — engaging kill-switch (active=${ledger.snapshot().active})`);
    ledger.kill();
  });
  server.listen(PORT, () => {
    console.log(
      `coding-gateway listening on :${PORT} (model=${config.model}, effort=${config.effort})`,
    );
    void readiness.check().then((report) => {
      if (!report.ready) console.error(`readiness failed: ${report.failed.join(',')}`);
    });
  });
}
