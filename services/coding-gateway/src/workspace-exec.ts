import { randomUUID } from 'node:crypto';
import { Agent, request } from 'node:https';
import { isIP } from 'node:net';
import { setTimeout as delay } from 'node:timers/promises';
import WebSocket from 'ws';
import { VAST_JUPYTER_CA } from './vast-jupyter-ca.js';

/**
 * Runs a command on a rented GPU workspace through its Jupyter server and
 * brings the declared output files back.
 *
 * The workspace URL carries a live Jupyter token, so it never leaves the
 * gateway: the model names a job it launched and this module talks to Jupyter
 * on its behalf. TLS is verified against Vast's Jupyter CA. Instances are
 * addressed by IP, so the chain is checked but the host name is not.
 *
 * The command is started detached and its completion is polled over the
 * contents API, so a long GPU job never depends on one WebSocket staying open.
 * Jupyter hides dotfiles from the contents API by default, which is why every
 * control file below has a plain name.
 */

const TOKEN = /^[A-Za-z0-9_-]{16,512}$/;
const KERNEL_ID = /^[A-Za-z0-9-]{1,128}$/;
const API_TIMEOUT_MS = 30_000;
const EXECUTE_TIMEOUT_MS = 60_000;
const POLL_INTERVAL_MS = 2_000;
const MARKER = '__covenant__';
const JOB_ROOT = 'covenant-jobs';
const TAIL_BYTES = 4_096;
// One base64 output at the per-run cap plus the contents API envelope.
const MAX_JSON_BYTES = 48 * 1024 * 1024;
const MAX_KERNEL_TEXT = 256 * 1024;

export const MAX_COMMAND_BYTES = 16 * 1024;
export const MAX_UPLOAD_FILES = 8;
export const MAX_UPLOAD_BYTES = 4 * 1024 * 1024;
export const MAX_OUTPUT_FILES = 8;
export const MAX_OUTPUT_BYTES = 32 * 1024 * 1024;

// Python, because the Jupyter kernel guarantees an interpreter and nothing else.
// The command sees that interpreter first on PATH, so python3 and pip are the
// kernel's even when the image's own Python is missing or different.
const RUN_SCRIPT = `import json, os, subprocess, sys, time
env = dict(os.environ, PYTHONUNBUFFERED='1', PIP_DISABLE_PIP_VERSION_CHECK='1', PIP_NO_INPUT='1')
env['PATH'] = os.path.dirname(sys.executable) + os.pathsep + env.get('PATH', '/usr/bin:/bin')
started = time.monotonic()
with open('covenant-stdout.log', 'wb') as out, open('covenant-stderr.log', 'wb') as err:
    code = subprocess.call(['bash', 'covenant-command.sh'], stdin=subprocess.DEVNULL, stdout=out, stderr=err, env=env)
with open('covenant-exit.tmp', 'w') as f:
    json.dump({'exit_code': code, 'duration_ms': round((time.monotonic() - started) * 1000)}, f)
os.replace('covenant-exit.tmp', 'covenant-exit.json')
`;

export class WorkspaceError extends Error {}

export interface WorkspaceEndpoint {
  origin: string;
  token: string;
}

export function parseWorkspaceUrl(raw: string): WorkspaceEndpoint {
  let url: URL;
  try {
    url = new URL(raw);
  } catch {
    throw new WorkspaceError('GPU workspace address is malformed');
  }
  const token = url.searchParams.get('token') ?? '';
  if (
    url.protocol !== 'https:' ||
    !isIP(url.hostname.replace(/^\[|\]$/g, '')) ||
    !url.port ||
    url.username ||
    url.password ||
    !TOKEN.test(token)
  ) {
    throw new WorkspaceError('GPU workspace address is not a Jupyter server this gateway accepts');
  }
  return { origin: url.origin, token };
}

/** Relative, visible path segments only: no traversal, no hidden files. */
export function safeRelativePath(path: unknown, label: string): string {
  if (
    typeof path !== 'string' ||
    path.length === 0 ||
    path.length > 128 ||
    !/^[A-Za-z0-9._/-]+$/.test(path) ||
    path.split('/').some((segment) => !segment || segment.startsWith('.')) ||
    path.startsWith('covenant-')
  ) {
    throw new WorkspaceError(
      `${label} must be a relative path of letters, digits, '.', '_', '-' and '/', without hidden or empty segments: ${JSON.stringify(path)}`,
    );
  }
  return path;
}

const contentsPath = (path: string): string =>
  `/api/contents/${path.split('/').map(encodeURIComponent).join('/')}`;

export interface KernelResult {
  ok: boolean;
  stdout: string;
  error?: string;
}

export interface WorkspaceClientOptions {
  /** PEM trust anchor. Tests pass their own; production uses Vast's CA. */
  ca?: string;
  timeoutMs?: number;
}

export class WorkspaceClient {
  private readonly agent: Agent;
  private readonly ca: string;
  private readonly timeoutMs: number;

  constructor(
    private readonly endpoint: WorkspaceEndpoint,
    options: WorkspaceClientOptions = {},
  ) {
    this.ca = options.ca ?? VAST_JUPYTER_CA;
    this.timeoutMs = options.timeoutMs ?? API_TIMEOUT_MS;
    this.agent = new Agent({
      ca: this.ca,
      checkServerIdentity: skipHostCheck,
      keepAlive: true,
      maxSockets: 4,
    });
  }

  close(): void {
    this.agent.destroy();
  }

  async json(
    method: string,
    path: string,
    body?: unknown,
  ): Promise<{ status: number; body: unknown }> {
    const url = new URL(path, this.endpoint.origin);
    const payload = body === undefined ? undefined : Buffer.from(JSON.stringify(body));
    return new Promise((resolve, reject) => {
      const req = request(
        url,
        {
          method,
          agent: this.agent,
          headers: {
            authorization: `token ${this.endpoint.token}`,
            accept: 'application/json',
            ...(payload
              ? { 'content-type': 'application/json', 'content-length': payload.length }
              : {}),
          },
          timeout: this.timeoutMs,
        },
        (res) => {
          const chunks: Buffer[] = [];
          let size = 0;
          res.on('data', (chunk: Buffer) => {
            size += chunk.length;
            if (size > MAX_JSON_BYTES) {
              req.destroy(new WorkspaceError('GPU workspace response exceeds the size limit'));
              return;
            }
            chunks.push(chunk);
          });
          res.on('end', () => {
            const text = Buffer.concat(chunks).toString('utf8');
            let parsed: unknown = null;
            try {
              parsed = text ? JSON.parse(text) : null;
            } catch {
              parsed = null;
            }
            resolve({ status: res.statusCode ?? 0, body: parsed });
          });
          res.on('error', reject);
        },
      );
      req.on('timeout', () => req.destroy(new WorkspaceError(`GPU workspace ${method} timed out`)));
      // Socket and TLS errors can name the host; report the failure without it.
      req.on('error', (cause) =>
        reject(
          cause instanceof WorkspaceError
            ? cause
            : new WorkspaceError(`GPU workspace request failed: ${errorCode(cause)}`),
        ),
      );
      req.end(payload);
    });
  }

  private async expect(method: string, path: string, body: unknown, ok: number[]) {
    const response = await this.json(method, path, body);
    if (!ok.includes(response.status))
      throw new WorkspaceError(`GPU workspace ${method} returned HTTP ${response.status}`);
    return response.body;
  }

  async makeDirectory(path: string): Promise<void> {
    const existing = await this.json('GET', `${contentsPath(path)}?content=0`);
    if (existing.status === 200) return;
    await this.expect('PUT', contentsPath(path), { type: 'directory' }, [200, 201]);
  }

  async upload(path: string, bytes: Buffer): Promise<void> {
    await this.expect(
      'PUT',
      contentsPath(path),
      { type: 'file', format: 'base64', content: bytes.toString('base64') },
      [200, 201],
    );
  }

  /** Null when the file does not exist yet. */
  async download(path: string): Promise<Buffer | null> {
    const response = await this.json(
      'GET',
      `${contentsPath(path)}?type=file&format=base64&content=1`,
    );
    if (response.status === 404) return null;
    const body = response.body as { content?: unknown; format?: unknown } | null;
    if (response.status !== 200 || body?.format !== 'base64' || typeof body.content !== 'string')
      throw new WorkspaceError(`GPU workspace download returned HTTP ${response.status}`);
    return Buffer.from(body.content, 'base64');
  }

  /** Runs Python in a fresh kernel whose working directory is `cwd`, then deletes the kernel. */
  async execute(code: string, cwd: string, timeoutMs = EXECUTE_TIMEOUT_MS): Promise<KernelResult> {
    const started = (await this.expect(
      'POST',
      '/api/kernels',
      { name: 'python3', path: cwd },
      [200, 201],
    )) as { id?: unknown } | null;
    const id = started?.id;
    if (typeof id !== 'string' || !KERNEL_ID.test(id))
      throw new WorkspaceError('GPU workspace returned a malformed kernel');
    try {
      return await this.executeOn(id, code, timeoutMs);
    } finally {
      await this.json('DELETE', `/api/kernels/${id}`).catch(() => undefined);
    }
  }

  private executeOn(kernelId: string, code: string, timeoutMs: number): Promise<KernelResult> {
    const session = randomUUID();
    const msgId = randomUUID();
    const origin = this.endpoint.origin.replace(/^https:/, 'wss:');
    const ws = new WebSocket(`${origin}/api/kernels/${kernelId}/channels?session_id=${session}`, {
      ca: this.ca,
      // Node treats any truthy return as a verification failure, so this must
      // return undefined; @types/ws declares it as returning boolean.
      checkServerIdentity:
        skipHostCheck as unknown as WebSocket.ClientOptions['checkServerIdentity'],
      headers: { authorization: `token ${this.endpoint.token}` },
      handshakeTimeout: this.timeoutMs,
      maxPayload: 8 * 1024 * 1024,
    });
    return new Promise((resolve, reject) => {
      let stdout = '';
      let error: string | undefined;
      let replied = false;
      let idle = false;
      let ok = false;
      const finish = (outcome: KernelResult | Error) => {
        clearTimeout(timer);
        ws.removeAllListeners();
        ws.on('error', () => undefined);
        ws.terminate();
        if (outcome instanceof Error) reject(outcome);
        else resolve(outcome);
      };
      const timer = setTimeout(
        () => finish(new WorkspaceError('GPU workspace kernel did not finish in time')),
        timeoutMs,
      );
      ws.on('open', () => {
        ws.send(
          JSON.stringify({
            header: {
              msg_id: msgId,
              username: 'covenant',
              session,
              msg_type: 'execute_request',
              version: '5.3',
              date: new Date().toISOString(),
            },
            parent_header: {},
            metadata: {},
            content: {
              code,
              silent: false,
              store_history: false,
              user_expressions: {},
              allow_stdin: false,
              stop_on_error: true,
            },
            buffers: [],
            channel: 'shell',
          }),
        );
      });
      ws.on('message', (data) => {
        let message: {
          channel?: string;
          msg_type?: string;
          parent_header?: { msg_id?: string };
          header?: { msg_type?: string };
          content?: Record<string, unknown>;
        };
        try {
          message = JSON.parse(String(data));
        } catch {
          return;
        }
        if (message.parent_header?.msg_id !== msgId) return;
        const type = message.msg_type ?? message.header?.msg_type;
        const content = message.content ?? {};
        if (type === 'stream' && content.name === 'stdout' && typeof content.text === 'string') {
          if (stdout.length < MAX_KERNEL_TEXT) stdout += content.text;
        } else if (type === 'error') {
          error = `${String(content.ename ?? 'Error')}: ${String(content.evalue ?? '')}`.slice(
            0,
            500,
          );
        } else if (type === 'execute_reply') {
          replied = true;
          ok = content.status === 'ok';
        } else if (type === 'status' && content.execution_state === 'idle') {
          idle = true;
        }
        if (replied && idle) finish({ ok, stdout, ...(error ? { error } : {}) });
      });
      ws.on('error', (cause) =>
        finish(new WorkspaceError(`GPU workspace kernel connection failed: ${errorCode(cause)}`)),
      );
      ws.on('close', () =>
        finish(new WorkspaceError('GPU workspace kernel connection closed before the reply')),
      );
    });
  }
}

/** The chain is verified against the CA; IP-addressed instances have no host name to match. */
function skipHostCheck(): undefined {
  return undefined;
}

function errorCode(cause: unknown): string {
  const code = (cause as { code?: unknown } | null)?.code;
  return typeof code === 'string' && /^[A-Z0-9_]{1,64}$/.test(code) ? code : 'network error';
}

/** Parses the one JSON line a control snippet prints after the marker. */
function marked(result: KernelResult, step: string): Record<string, unknown> {
  if (!result.ok)
    throw new WorkspaceError(`GPU workspace ${step} failed: ${result.error ?? 'error'}`);
  const line = result.stdout
    .split('\n')
    .reverse()
    .find((text) => text.startsWith(MARKER));
  try {
    const value = JSON.parse(line?.slice(MARKER.length) ?? '');
    if (value && typeof value === 'object' && !Array.isArray(value))
      return value as Record<string, unknown>;
  } catch {
    // fall through
  }
  throw new WorkspaceError(`GPU workspace ${step} returned no result`);
}

// JSON string literals are valid Python string literals for these ASCII values.
const enterJob = (dir: string, id: string): string =>
  [
    'import json, os',
    `if os.path.basename(os.getcwd()) != ${JSON.stringify(id)}:`,
    `    os.chdir(${JSON.stringify(dir)})`,
  ].join('\n');

const startCode = (dir: string, id: string): string =>
  [
    enterJob(dir, id),
    'import subprocess, sys',
    "p = subprocess.Popen([sys.executable, 'covenant-run.py'], start_new_session=True,",
    '    stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)',
    "open('covenant.pid', 'w').write(str(p.pid))",
    `print(${JSON.stringify(MARKER)} + json.dumps({'pid': p.pid}))`,
  ].join('\n');

const killCode = (dir: string, id: string): string =>
  [
    enterJob(dir, id),
    'import signal',
    'try:',
    "    os.killpg(int(open('covenant.pid').read()), signal.SIGKILL)",
    'except (ProcessLookupError, FileNotFoundError, ValueError):',
    '    pass',
    `print(${JSON.stringify(MARKER)} + '{}')`,
  ].join('\n');

const collectCode = (dir: string, id: string, outputs: string[]): string =>
  [
    enterJob(dir, id),
    'def tail(name):',
    '    try:',
    "        with open(name, 'rb') as f:",
    '            f.seek(0, 2)',
    `            f.seek(max(0, f.tell() - ${TAIL_BYTES}))`,
    "            return f.read().decode('utf-8', 'replace')",
    '    except FileNotFoundError:',
    "        return ''",
    'sizes = {}',
    `for name in ${JSON.stringify(outputs)}:`,
    '    sizes[name] = os.path.getsize(name) if os.path.isfile(name) and not os.path.islink(name) else None',
    `print(${JSON.stringify(MARKER)} + json.dumps({'stdout': tail('covenant-stdout.log'), 'stderr': tail('covenant-stderr.log'), 'sizes': sizes}))`,
  ].join('\n');

function parseExit(raw: Buffer): { exit_code?: unknown; duration_ms?: unknown } {
  try {
    const value = JSON.parse(raw.toString('utf8'));
    if (value && typeof value === 'object') return value;
  } catch {
    // The file is renamed into place complete, so this means it was tampered with.
  }
  return { exit_code: null };
}

export interface WorkspaceJobSpec {
  command: string;
  files: Array<{ path: string; bytes: Buffer }>;
  outputs: string[];
  /** Absolute time by which the command must have finished or been stopped. */
  deadlineMs: number;
}

export interface WorkspaceJobOutcome {
  exitCode: number | null;
  timedOut: boolean;
  durationMs: number;
  stdoutTail: string;
  stderrTail: string;
  outputs: Array<{ path: string; bytes: Buffer }>;
  missing: string[];
  skipped: string[];
}

export async function runWorkspaceJob(
  client: Pick<WorkspaceClient, 'makeDirectory' | 'upload' | 'download' | 'execute'>,
  spec: WorkspaceJobSpec,
  options: { signal?: AbortSignal; now?: () => number; pollMs?: number } = {},
): Promise<WorkspaceJobOutcome> {
  const now = options.now ?? Date.now;
  const id = randomUUID();
  const dir = `${JOB_ROOT}/${id}`;
  await client.makeDirectory(JOB_ROOT);
  await client.makeDirectory(dir);
  for (const file of spec.files) {
    const parts = file.path.split('/');
    for (let i = 1; i < parts.length; i++)
      await client.makeDirectory(`${dir}/${parts.slice(0, i).join('/')}`);
    await client.upload(`${dir}/${file.path}`, file.bytes);
  }
  await client.upload(`${dir}/covenant-command.sh`, Buffer.from(`${spec.command}\n`));
  await client.upload(`${dir}/covenant-run.py`, Buffer.from(RUN_SCRIPT));

  const startedAt = now();
  marked(await client.execute(startCode(dir, id), dir), 'start');

  let exit: { exit_code?: unknown; duration_ms?: unknown } | null = null;
  while (!exit) {
    if (options.signal?.aborted) break;
    const remaining = spec.deadlineMs - now();
    if (remaining <= 0) break;
    await delay(Math.min(options.pollMs ?? POLL_INTERVAL_MS, remaining), undefined, {
      signal: options.signal,
    }).catch(() => undefined);
    const raw = await client.download(`${dir}/covenant-exit.json`);
    if (raw) exit = parseExit(raw);
  }
  if (!exit) {
    // The process group was started in its own session; stop all of it.
    marked(await client.execute(killCode(dir, id), dir), 'stop');
    if (options.signal?.aborted)
      throw new WorkspaceError('run aborted while the GPU command was running');
  }

  const collected = marked(
    await client.execute(collectCode(dir, id, spec.outputs), dir),
    'collect',
  );
  const sizes = (collected.sizes ?? {}) as Record<string, unknown>;
  const outputs: WorkspaceJobOutcome['outputs'] = [];
  const missing: string[] = [];
  const skipped: string[] = [];
  let total = 0;
  for (const path of spec.outputs) {
    const size = sizes[path];
    if (typeof size !== 'number' || !Number.isSafeInteger(size) || size < 0) {
      missing.push(path);
      continue;
    }
    if (total + size > MAX_OUTPUT_BYTES) {
      skipped.push(path);
      continue;
    }
    const bytes = await client.download(`${dir}/${path}`);
    if (!bytes || bytes.length !== size) {
      // Changed after the size check: report it rather than return a partial file.
      missing.push(path);
      continue;
    }
    total += size;
    outputs.push({ path, bytes });
  }

  const exitCode = exit && Number.isSafeInteger(exit.exit_code) ? (exit.exit_code as number) : null;
  return {
    exitCode,
    timedOut: !exit,
    durationMs:
      exit && Number.isSafeInteger(exit.duration_ms)
        ? (exit.duration_ms as number)
        : now() - startedAt,
    stdoutTail: String(collected.stdout ?? ''),
    stderrTail: String(collected.stderr ?? ''),
    outputs,
    missing,
    skipped,
  };
}
