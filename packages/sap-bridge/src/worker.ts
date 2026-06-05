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
//   stats                           — cumulative RPC usage counters (no network)
//   publish-agent     <stdin: AgentManifest>
//   update-agent      <stdin: AgentManifest>      — replaces all fields
//   attest-root       <stdin: AuditRootAttestation>  — self-anchored ledger
//   attest-agent      <stdin: AgentAttestation>      — cross-party (verifier)
//   find-agent        <stdin: { pda }>            — discovery projection
//   describe-agent    <stdin: { pda }>            — full agent projection
//   find-by-protocol  <stdin: { protocol, limit? }>

import { SapBridge, resolveSynapseConfig, type SapKeypair } from './index.js';
import { loadKeypairFromFile } from './keypair.js';
import { readStats, recordCall } from './stats.js';

// Commands that issue Solana RPC calls — recorded in the usage counters.
// `status` and `stats` are pure-local and excluded.
const NETWORK_COMMANDS = new Set([
  'publish-agent',
  'update-agent',
  'attest-root',
  'attest-agent',
  'find-agent',
  'describe-agent',
  'find-by-protocol',
]);

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

async function loadVerifier(): Promise<SapKeypair | undefined> {
  const path = process.env.COVENANT_SAP_VERIFIER_KEYPAIR;
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

// Dispatch a network-bearing command. Kept separate from main() so the
// single call site there can wrap every RPC op in usage accounting.
async function dispatchNetwork(bridge: SapBridge, command: string): Promise<unknown> {
  switch (command) {
    case 'publish-agent':
      return bridge.publishAgent(await parsePayload());
    case 'update-agent':
      return bridge.updateAgent(await parsePayload());
    case 'attest-root':
      return bridge.publishAuditRoot(await parsePayload());
    case 'attest-agent':
      return bridge.attestAgent(await parsePayload());
    case 'find-agent': {
      const { pda } = await parsePayload<{ pda?: string }>();
      if (!pda) throw new Error('find-agent: missing "pda" in payload');
      return bridge.findAgentByPda(pda);
    }
    case 'describe-agent': {
      const { pda } = await parsePayload<{ pda?: string }>();
      if (!pda) throw new Error('describe-agent: missing "pda" in payload');
      return bridge.describeAgent(pda);
    }
    case 'find-by-protocol': {
      const { protocol, limit } = await parsePayload<{ protocol?: string; limit?: number }>();
      if (!protocol) throw new Error('find-by-protocol: missing "protocol" in payload');
      return bridge.findAgentsByProtocol(protocol, limit);
    }
    default:
      throw new Error(
        `unknown command '${command}'. Expected: status | stats | publish-agent | ` +
          'update-agent | attest-root | attest-agent | find-agent | describe-agent | find-by-protocol',
      );
  }
}

async function main(): Promise<void> {
  const command = process.argv[2] ?? '';
  const config = resolveSynapseConfig(process.env);

  // status / stats never touch the network or a signer; resolve first
  // and skip usage accounting for them.
  if (command === 'status') {
    const bridge = new SapBridge({ config });
    emit(bridge.status());
    return;
  }
  if (command === 'stats') {
    emit(readStats());
    return;
  }

  const bridge = new SapBridge({
    config,
    signer: await loadSigner(),
    verifier: await loadVerifier(),
  });

  // Record usage only for genuine RPC commands. An unknown command falls
  // through to dispatchNetwork's throw without being miscounted.
  if (!NETWORK_COMMANDS.has(command)) {
    emit(await dispatchNetwork(bridge, command));
    return;
  }
  try {
    const data = await dispatchNetwork(bridge, command);
    recordCall(true);
    emit(data);
  } catch (err) {
    recordCall(false);
    throw err;
  }
}

main().catch(fail);
