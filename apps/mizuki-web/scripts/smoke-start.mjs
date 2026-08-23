import { spawn } from 'node:child_process';
import { once } from 'node:events';
import { createServer as createHttpServer, request } from 'node:http';
import { createServer as createTcpServer } from 'node:net';
import { fileURLToPath } from 'node:url';

const missingId = '00000000-0000-4000-8000-000000000000';
const outageId = '00000000-0000-4000-8000-000000000001';
const validId = '00000000-0000-4000-8000-000000000002';
const timestamp = '2026-08-23T00:00:00.000Z';

const api = createHttpServer((req, res) => {
  const url = new URL(req.url || '/', 'http://127.0.0.1');
  const match = /^\/v1\/(jobs|bounties)\/([^/]+)$/.exec(url.pathname);
  if (!match) return json(res, 404, { error: 'not found' });

  const [, resource, id] = match;
  if (id === missingId) return json(res, 404, { error: 'not found' });
  if (id === outageId) return json(res, 503, { error: 'upstream unavailable' });
  if (id !== validId) return json(res, 404, { error: 'not found' });
  return json(res, 200, resource === 'jobs' ? jobFixture() : bountyFixture());
});

api.listen(0, '127.0.0.1');
await once(api, 'listening');
const apiAddress = api.address();
if (!apiAddress || typeof apiAddress === 'string') throw new Error('failed to bind fake API');

const port = await availablePort();
const appRoot = fileURLToPath(new URL('..', import.meta.url));
const env = {
  ...Object.fromEntries(
    Object.entries(process.env).filter(
      ([key]) => !/^(MIZUKI_|NEXT_PUBLIC_MIZUKI_|PORT$|HOSTNAME$)/.test(key),
    ),
  ),
  NODE_ENV: 'production',
  HOSTNAME: '127.0.0.1',
  PORT: String(port),
  MIZUKI_API_URL: `http://127.0.0.1:${apiAddress.port}`,
  MIZUKI_DEMO_MODE: '0',
  MIZUKI_WEB_PROXY_SECRET: 'p'.repeat(32),
};

const child = spawn(process.execPath, ['.next/standalone/apps/mizuki-web/server.js'], {
  cwd: appRoot,
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

  for (const resource of ['jobs', 'bounties']) {
    for (const headers of [{ 'user-agent': 'mizuki-web-smoke' }, {}]) {
      const response = await page(port, `/${resource}/${missingId}`, headers);
      assert(response.status === 404, `${resource} missing receipt returned ${response.status}`);
    }

    const outage = await page(port, `/${resource}/${outageId}`, {});
    assert(outage.status === 200, `${resource} outage returned ${outage.status}`);
    assert(
      outage.body.includes(resource === 'jobs' ? 'Job receipt unavailable' : 'Bounty unavailable'),
      `${resource} outage did not render its unavailable state`,
    );

    const valid = await page(port, `/${resource}/${validId}`, {});
    assert(valid.status === 200, `${resource} valid receipt returned ${valid.status}`);
    assert(valid.body.includes(validId), `${resource} valid receipt did not render its identifier`);
  }

  child.kill('SIGTERM');
  const [code, signal] = await withTimeout(once(child, 'exit'), 5_000, 'web shutdown');
  if (code !== 0 && code !== 143 && signal !== 'SIGTERM') {
    throw new Error(`web shutdown failed (${code ?? signal})\n${output}`);
  }
  process.stdout.write('Mizuki web smoke OK\n');
} finally {
  if (child.exitCode === null) child.kill('SIGKILL');
  await new Promise((resolve, reject) => {
    api.close((cause) => (cause ? reject(cause) : resolve()));
  });
}

function json(res, status, body) {
  const payload = JSON.stringify(body);
  res.writeHead(status, {
    'content-type': 'application/json',
    'content-length': Buffer.byteLength(payload),
  });
  res.end(payload);
}

function jobFixture() {
  return {
    id: validId,
    state: 'delivered',
    issueUrl: 'https://github.com/example/project/issues/1',
    class: 'micro',
    priceAtomic: '2000000',
    changedFiles: ['README.md'],
    validations: [{ command: 'pnpm test', exitCode: 0 }],
    variableRouteCostEstimateUsd: 0.5,
    costCoverage: {
      included: [
        'gateway_model_token_rate_estimate',
        'gateway_sandbox_runtime_estimate',
        'reviewer_model_token_rate_estimate',
      ],
      excluded: ['provider_billing_adjustments', 'chain_and_facilitator_fees', 'infrastructure'],
    },
    createdAt: timestamp,
    updatedAt: timestamp,
  };
}

function bountyFixture() {
  return {
    id: validId,
    title: 'Smoke-test rescue bounty',
    repository: 'example/project',
    issueUrl: 'https://github.com/example/project/issues/1',
    issueNumber: 1,
    amountUsd: 2,
    state: 'draft',
    failureClass: 'validation_failed',
    acceptanceCriteria: ['The production receipt renders.'],
    createdAt: timestamp,
    updatedAt: timestamp,
  };
}

async function availablePort() {
  const server = createTcpServer();
  server.listen(0, '127.0.0.1');
  await once(server, 'listening');
  const address = server.address();
  if (!address || typeof address === 'string') throw new Error('failed to reserve a web port');
  await new Promise((resolve) => server.close(resolve));
  return address.port;
}

async function waitForHealth(port, process) {
  const deadline = Date.now() + 10_000;
  while (Date.now() < deadline) {
    if (process.exitCode !== null) throw new Error(`web exited before health check\n${output}`);
    try {
      const response = await page(port, '/healthz', {});
      if (response.status === 200) return;
    } catch {
      await delay(50);
    }
  }
  throw new Error(`web health check timed out\n${output}`);
}

function page(port, path, headers) {
  return new Promise((resolve, reject) => {
    const req = request({ hostname: '127.0.0.1', port, path, method: 'GET', headers }, (res) => {
      let body = '';
      res.setEncoding('utf8');
      res.on('data', (chunk) => {
        body += chunk;
      });
      res.on('end', () => resolve({ status: res.statusCode, body }));
    });
    req.setTimeout(5_000, () => req.destroy(new Error(`request timed out: ${path}`)));
    req.on('error', reject);
    req.end();
  });
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

function assert(condition, message) {
  if (!condition) throw new Error(`${message}\n${output}`);
}

function delay(ms) {
  return new Promise((resolve) => setTimeout(resolve, ms));
}
