#!/usr/bin/env node
// Covenant SAP bridge worker.
//
// The daemon (covenant-sap-bridge crate) holds no JS runtime and no
// SDK, so it shells out to this worker for every on-chain operation.
// The worker is also a usable CLI on its own.
//
// Protocol: argv[2] is the command. A JSON payload may be supplied on
// stdin. Exactly one JSON object is written to stdout:
//   { "ok": true, "data": <result> }
//   { "ok": false, "error": "<message>", "name": "<ErrorName>" }
// A non-zero exit code accompanies any { ok: false }.
//
// Config comes from the environment via resolveSynapseConfig (the same
// COVENANT_SAP_* layering the rest of Covenant uses). The signer, when
// required, is loaded from the keypair file at COVENANT_SAP_KEYPAIR.
//
// Commands:
//   status                          — resolved config snapshot (no network)
//   publish-agent     <stdin: AgentManifest>
//   update-agent      <stdin: AgentManifest>      — replaces all fields
//   attest-root       <stdin: AuditRootAttestation>
//   find-agent        <stdin: { pda }>            — discovery projection
//   describe-agent    <stdin: { pda }>            — full agent projection
//   find-by-protocol  <stdin: { protocol }>

import { SapBridge, resolveSynapseConfig, type SapKeypair } from './index.js';
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
    throw new Error('synapse bridge worker: stdin is not valid JSON', { cause });
  }
}

async function loadSigner(): Promise<SapKeypair | undefined> {
  const path = process.env.COVENANT_SAP_KEYPAIR;
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

async function main(): Promise<void> {
  const command = process.argv[2];
  const config = resolveSynapseConfig(process.env);

  // status never needs a signer or the network, so resolve it first.
  if (command === 'status') {
    const bridge = new SapBridge({ config });
    emit(bridge.status());
    return;
  }

  const bridge = new SapBridge({ config, signer: await loadSigner() });

  switch (command) {
    case 'publish-agent': {
      emit(await bridge.publishAgent(await parsePayload()));
      return;
    }
    case 'update-agent': {
      emit(await bridge.updateAgent(await parsePayload()));
      return;
    }
    case 'attest-root': {
      emit(await bridge.publishAuditRoot(await parsePayload()));
      return;
    }
    case 'find-agent': {
      const { pda } = await parsePayload<{ pda?: string }>();
      if (!pda) throw new Error('find-agent: missing "pda" in payload');
      emit(await bridge.findAgentByPda(pda));
      return;
    }
    case 'describe-agent': {
      const { pda } = await parsePayload<{ pda?: string }>();
      if (!pda) throw new Error('describe-agent: missing "pda" in payload');
      emit(await bridge.describeAgent(pda));
      return;
    }
    case 'find-by-protocol': {
      const { protocol } = await parsePayload<{ protocol?: string }>();
      if (!protocol) throw new Error('find-by-protocol: missing "protocol" in payload');
      emit(await bridge.findAgentsByProtocol(protocol));
      return;
    }
    default:
      throw new Error(
        `unknown command '${command ?? ''}'. Expected: status | publish-agent | ` +
          'update-agent | attest-root | find-agent | describe-agent | find-by-protocol',
      );
  }
}

main().catch(fail);
