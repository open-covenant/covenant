#!/usr/bin/env node
// covenant-said-worker. Run as a subprocess by the Rust bridge; also
// usable from the shell.
//
// argv[2] is the command. Payload on stdin. Exactly one envelope on stdout:
//   { "ok": true, "data": <result> }
//   { "ok": false, "error": "<message>", "name": "<ErrorName>" }
//
// Config: COVENANT_SAID_*. Signer: COVENANT_SAID_KEYPAIR (Solana CLI JSON).
//
// Commands:
//   status            no payload. Resolved config + signer presence.
//   register-agent    { metadataUri }
//   get-verified      {}
//   submit-anchor     { anchorIndex, startSeq, endSeq, merkleRootHex }
//   validate-work     { agent, taskHashHex, passed, evidenceUri }

import { SaidBridge, resolveSaidConfig } from './index.js';
import { loadKeypairFromFile } from './keypair.js';

const MAX_STDIN_BYTES = 1 << 20;
const DEFAULT_STDIN_TIMEOUT_MS = 10_000;

function stdinTimeoutMs(): number {
  const raw = process.env.COVENANT_SAID_STDIN_TIMEOUT_MS;
  const n = raw ? Number(raw) : NaN;
  return Number.isFinite(n) && n > 0 ? n : DEFAULT_STDIN_TIMEOUT_MS;
}

async function readStdin(): Promise<string> {
  if (process.stdin.isTTY) return '';
  let total = 0;
  const chunks: Buffer[] = [];
  const read = (async () => {
    for await (const chunk of process.stdin) {
      const buf = chunk as Buffer;
      total += buf.length;
      if (total > MAX_STDIN_BYTES) {
        throw new Error(`said bridge worker: stdin exceeds ${MAX_STDIN_BYTES} bytes`);
      }
      chunks.push(buf);
    }
    return Buffer.concat(chunks).toString('utf8').trim();
  })();
  const ms = stdinTimeoutMs();
  const timeout = new Promise<never>((_, reject) =>
    setTimeout(
      () => reject(new Error(`said bridge worker: stdin read timed out after ${ms}ms`)),
      ms,
    ).unref(),
  );
  return Promise.race([read, timeout]);
}

class BridgePayloadError extends Error {
  constructor(message: string, cause?: unknown) {
    super(message);
    this.name = 'BridgePayloadError';
    if (cause !== undefined) (this as { cause?: unknown }).cause = cause;
  }
}

const URL_MAX = 200;

function requireJsonPayload<T>(command: string, raw: string): T {
  if (!raw) {
    throw new BridgePayloadError(
      `${command}: expected a JSON payload on stdin (got empty input)`,
    );
  }
  try {
    return JSON.parse(raw) as T;
  } catch (cause) {
    throw new BridgePayloadError(`${command}: stdin is not valid JSON`, cause);
  }
}

function validateRegisterAgent(payload: unknown): { metadataUri: string } {
  if (!payload || typeof payload !== 'object') {
    throw new BridgePayloadError('register-agent: payload must be an object');
  }
  const uri = (payload as { metadataUri?: unknown }).metadataUri;
  if (typeof uri !== 'string' || uri.length === 0) {
    throw new BridgePayloadError('register-agent: metadataUri must be a non-empty string');
  }
  if (uri.length > URL_MAX) {
    throw new BridgePayloadError(`register-agent: metadataUri exceeds ${URL_MAX} chars`);
  }
  try {
    const parsed = new URL(uri);
    if (parsed.protocol !== 'http:' && parsed.protocol !== 'https:') {
      throw new Error('non-http(s) protocol');
    }
  } catch (cause) {
    throw new BridgePayloadError('register-agent: metadataUri is not a valid http(s) URL', cause);
  }
  return { metadataUri: uri };
}

async function loadSigner(): Promise<Awaited<ReturnType<typeof loadKeypairFromFile>> | undefined> {
  const path = process.env.COVENANT_SAID_KEYPAIR;
  if (!path) return undefined;
  return loadKeypairFromFile(path);
}

async function signerFileReadable(): Promise<boolean> {
  const path = process.env.COVENANT_SAID_KEYPAIR?.trim();
  if (!path) return false;
  try {
    const { statSync } = await import('node:fs');
    const s = statSync(path);
    return s.isFile();
  } catch {
    return false;
  }
}

function jsonReplacer(_key: string, value: unknown): unknown {
  return typeof value === 'bigint' ? value.toString() : value;
}

function emit(data: unknown): void {
  process.stdout.write(JSON.stringify({ ok: true, data }, jsonReplacer) + '\n');
}

function fail(err: unknown): never {
  const error = err instanceof Error ? err.message : String(err);
  const name = err instanceof Error ? err.name : 'Error';
  process.stdout.write(JSON.stringify({ ok: false, error, name }) + '\n');
  process.exit(1);
}

async function dispatch(bridge: SaidBridge, command: string): Promise<unknown> {
  switch (command) {
    case 'register-agent': {
      const raw = await readStdin();
      const payload = requireJsonPayload<unknown>('register-agent', raw);
      return bridge.registerAgent(validateRegisterAgent(payload));
    }
    case 'get-verified':
      return bridge.getVerified();
    case 'submit-anchor': {
      const raw = await readStdin();
      const payload = requireJsonPayload<unknown>('submit-anchor', raw);
      return bridge.submitAnchor(payload as Parameters<SaidBridge['submitAnchor']>[0]);
    }
    case 'validate-work': {
      const raw = await readStdin();
      const payload = requireJsonPayload<unknown>('validate-work', raw);
      return bridge.validateWork(payload as Parameters<SaidBridge['validateWork']>[0]);
    }
    default:
      throw new Error(
        `unknown command '${command}'. Expected: status | register-agent | get-verified | ` +
          'submit-anchor | validate-work',
      );
  }
}

async function main(): Promise<void> {
  const command = process.argv[2] ?? '';
  const config = resolveSaidConfig(process.env);

  if (command === 'status') {
    emit({
      ...config,
      hasSigner: await signerFileReadable(),
    });
    return;
  }

  const bridge = new SaidBridge({
    config,
    signer: await loadSigner(),
  });
  emit(await dispatch(bridge, command));
}

main().catch(fail);
