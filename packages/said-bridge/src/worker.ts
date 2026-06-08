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

async function readStdin(): Promise<string> {
  if (process.stdin.isTTY) return '';
  const chunks: Buffer[] = [];
  for await (const chunk of process.stdin) {
    chunks.push(chunk as Buffer);
  }
  return Buffer.concat(chunks).toString('utf8').trim();
}

async function parsePayload<T>(): Promise<T> {
  const raw = await readStdin();
  if (!raw) return {} as T;
  try {
    return JSON.parse(raw) as T;
  } catch (cause) {
    throw new Error('said bridge worker: stdin is not valid JSON', { cause });
  }
}

async function loadSigner(): Promise<Awaited<ReturnType<typeof loadKeypairFromFile>> | undefined> {
  const path = process.env.COVENANT_SAID_KEYPAIR;
  if (!path) return undefined;
  return loadKeypairFromFile(path);
}

function emit(data: unknown): void {
  process.stdout.write(JSON.stringify({ ok: true, data }) + '\n');
}

function fail(err: unknown): never {
  const error = err instanceof Error ? err.message : String(err);
  const name = err instanceof Error ? err.name : 'Error';
  process.stdout.write(JSON.stringify({ ok: false, error, name }) + '\n');
  process.exit(1);
}

async function dispatch(bridge: SaidBridge, command: string): Promise<unknown> {
  switch (command) {
    case 'register-agent':
      return bridge.registerAgent(await parsePayload());
    case 'get-verified':
      return bridge.getVerified();
    case 'submit-anchor':
      return bridge.submitAnchor(await parsePayload());
    case 'validate-work':
      return bridge.validateWork(await parsePayload());
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
      hasSigner: Boolean(process.env.COVENANT_SAID_KEYPAIR?.trim()),
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
