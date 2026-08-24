import { spawn } from 'node:child_process';
import { once } from 'node:events';
import { createServer } from 'node:net';

const port = await availablePort();
const env = {
  ...Object.fromEntries(
    Object.entries(process.env).filter(([key]) => !/^(MIZUKI_|USEPOD_|CLAWPUMP_)/.test(key)),
  ),
  NODE_ENV: 'test',
  MIZUKI_HOST: '127.0.0.1',
  MIZUKI_PAYMENT_MODE: 'mock',
  MIZUKI_PORT: String(port),
};

const child = spawn(process.execPath, ['dist/server.js'], {
  cwd: new URL('..', import.meta.url),
  env,
  stdio: ['ignore', 'pipe', 'pipe'],
});

let output = '';
for (const stream of [child.stdout, child.stderr]) {
  stream.setEncoding('utf8');
  stream.on('data', (chunk) => {
    output = `${output}${chunk}`.slice(-32_000);
  });
}

try {
  await waitForHealth(port, child);
  await delay(100);
  if (child.exitCode !== null) throw new Error(`server exited early\n${output}`);

  child.kill('SIGTERM');
  const [code, signal] = await withTimeout(once(child, 'exit'), 5_000, 'server shutdown');
  if (code !== 0 || signal !== null) {
    throw new Error(`server shutdown failed (${code ?? signal})\n${output}`);
  }
  process.stdout.write('Mizuki server smoke OK\n');
} finally {
  if (child.exitCode === null) child.kill('SIGKILL');
}

async function availablePort() {
  const server = createServer();
  server.listen(0, '127.0.0.1');
  await once(server, 'listening');
  const address = server.address();
  if (!address || typeof address === 'string') throw new Error('failed to reserve a smoke port');
  await new Promise((resolve) => server.close(resolve));
  return address.port;
}

async function waitForHealth(port, process) {
  const deadline = Date.now() + 5_000;
  while (Date.now() < deadline) {
    if (process.exitCode !== null) throw new Error(`server exited before health check\n${output}`);
    try {
      const response = await fetch(`http://127.0.0.1:${port}/healthz`, {
        signal: AbortSignal.timeout(500),
      });
      if (response.status === 200 && (await response.json()).ok === true) return;
    } catch {
      await delay(50);
    }
  }
  throw new Error(`server health check timed out\n${output}`);
}

async function withTimeout(promise, timeoutMs, action) {
  let timer;
  try {
    return await Promise.race([
      promise,
      new Promise((_, reject) => {
        timer = setTimeout(() => reject(new Error(`${action} timed out`)), timeoutMs);
      }),
    ]);
  } finally {
    if (timer) clearTimeout(timer);
  }
}

function delay(ms) {
  return new Promise((resolve) => setTimeout(resolve, ms));
}
