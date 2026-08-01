import assert from 'node:assert/strict';
import {createHash, generateKeyPairSync, sign} from 'node:crypto';
import {spawn} from 'node:child_process';
import net from 'node:net';
import {Client} from '@modelcontextprotocol/sdk/client/index.js';
import {StdioClientTransport} from '@modelcontextprotocol/sdk/client/stdio.js';
import {StreamableHTTPClientTransport} from '@modelcontextprotocol/sdk/client/streamableHttp.js';
import bs58 from 'bs58';

const canonical = (value) => {
  if (value === null || typeof value !== 'object') return JSON.stringify(value);
  if (Array.isArray(value)) return `[${value.map(canonical).join(',')}]`;
  const entries = Object.entries(value)
    .filter(([, current]) => current !== undefined)
    .sort(([left], [right]) => left.localeCompare(right));
  return `{${entries.map(([key, current]) => `${JSON.stringify(key)}:${canonical(current)}`).join(',')}}`;
};

function makeAttestation() {
  const {publicKey, privateKey} = generateKeyPairSync('ed25519');
  const payload = {subject: 'did:example:agent', claim: {role: 'worker'}, ts: 1_750_000_000};
  const digest = createHash('sha256').update(canonical(payload), 'utf8').digest('hex');
  const signature = sign(
    null,
    Buffer.from(`covenant.attest.v1\n${digest}`, 'utf8'),
    privateKey,
  );
  const jwk = publicKey.export({format: 'jwk'});
  return {
    alg: 'ed25519',
    domain: 'covenant.attest.v1',
    canonicalization: 'JSON, recursively key-sorted, no insignificant whitespace, UTF-8',
    payload,
    digest_sha256_hex: digest,
    pubkey_b58: bs58.encode(Buffer.from(jwk.x, 'base64url')),
    signature_b58: bs58.encode(signature),
  };
}

async function exercise(client, live) {
  const tools = await client.listTools();
  const names = tools.tools.map((tool) => tool.name).sort();
  assert.deepEqual(names, [
    'covenant_agent_passport',
    'covenant_payment_history',
    'covenant_verify',
  ]);
  assert.ok(tools.tools.every((tool) => tool.annotations?.readOnlyHint));

  const attestation = makeAttestation();
  const valid = await client.callTool({
    name: 'covenant_verify',
    arguments: {attestation, expected_signer: attestation.pubkey_b58},
  });
  assert.notEqual(valid.isError, true);
  assert.deepEqual(valid.structuredContent, {
    ok: true,
    subject: 'did:example:agent',
    signer: attestation.pubkey_b58,
    signatureValid: true,
    signerMatches: true,
  });

  const tampered = {
    ...attestation,
    payload: {...attestation.payload, subject: 'did:example:attacker'},
  };
  const rejected = await client.callTool({
    name: 'covenant_verify',
    arguments: {attestation: tampered},
  });
  assert.equal(rejected.isError, true);

  const emptyExpectedSigner = await client.callTool({
    name: 'covenant_verify',
    arguments: {attestation, expected_signer: ''},
  });
  assert.equal(emptyExpectedSigner.isError, true);

  const badAddress = await client.callTool({
    name: 'covenant_payment_history',
    arguments: {wallet: 'not-an-address'},
  });
  assert.equal(badAddress.isError, true);

  if (live) {
    const history = await client.callTool({
      name: 'covenant_payment_history',
      arguments: {wallet: '8xbXHAhiVe2BrYDq4qpTA5SSYJG9XNjNN6jcrudhTKCM'},
    });
    assert.notEqual(history.isError, true);

    const passport = await client.callTool({
      name: 'covenant_agent_passport',
      arguments: {asset: '4XtUrwvPWAzMGnsKenMpTMATXN3e2quJV11Jg2dab2dc'},
    });
    assert.notEqual(passport.isError, true);
  }
}

function freePort() {
  return new Promise((resolve, reject) => {
    const server = net.createServer();
    server.once('error', reject);
    server.listen(0, '127.0.0.1', () => {
      const address = server.address();
      if (!address || typeof address === 'string') {
        server.close();
        reject(new Error('could not allocate a test port'));
        return;
      }
      server.close((error) => (error ? reject(error) : resolve(address.port)));
    });
  });
}

async function waitForHealth(url, child) {
  const deadline = Date.now() + 8_000;
  while (Date.now() < deadline) {
    if (child.exitCode !== null) throw new Error(`HTTP server exited with ${child.exitCode}`);
    try {
      const response = await fetch(url);
      if (response.ok) return;
    } catch {
      // The child is still starting.
    }
    await new Promise((resolve) => setTimeout(resolve, 50));
  }
  throw new Error('HTTP server did not become healthy');
}

const live = process.argv.includes('--live');
if (live && !process.env.COVENANT_SOLANA_MAINNET_RPC_URL) {
  throw new Error('COVENANT_SOLANA_MAINNET_RPC_URL is required for --live');
}

const stdioTransport = new StdioClientTransport({
  command: process.execPath,
  args: ['dist/server.js', '--stdio'],
  env: {...process.env},
});
const stdioClient = new Client({name: 'covenant-trust-stdio-test', version: '1.0.0'});
await stdioClient.connect(stdioTransport);
try {
  await exercise(stdioClient, live);
} finally {
  await stdioClient.close();
}

const port = await freePort();
const child = spawn(process.execPath, ['dist/server.js'], {
  env: {...process.env, PORT: String(port)},
  stdio: ['ignore', 'ignore', 'pipe'],
});
let stderr = '';
child.stderr.setEncoding('utf8');
child.stderr.on('data', (chunk) => {
  stderr += chunk;
});

try {
  await waitForHealth(`http://127.0.0.1:${port}/health`, child);
  const httpTransport = new StreamableHTTPClientTransport(
    new URL(`http://127.0.0.1:${port}/mcp`),
  );
  const httpClient = new Client({name: 'covenant-trust-http-test', version: '1.0.0'});
  await httpClient.connect(httpTransport);
  try {
    await exercise(httpClient, false);
  } finally {
    await httpClient.close().catch(() => undefined);
  }
} catch (error) {
  throw new Error(`${error instanceof Error ? error.message : String(error)}\n${stderr}`);
} finally {
  child.kill('SIGTERM');
}

console.log(`covenant-trust smoke passed (stdio + HTTP${live ? ' + live Solana' : ''})`);
