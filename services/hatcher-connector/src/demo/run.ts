// End-to-end demo: a mock Hatcher mesh dispatches a developer task DOWN into the
// connector, which mints least-privilege grants, runs it as a local covenant intent,
// relays the live trace, and folds the result + audit root into a connector-trace
// proof sent back UP. Prints the trace and the proof envelope.
//
//   OFFLINE (default): scripted daemon, deterministic, no deps.
//   LIVE: set COVENANT_DAEMON_URL + COVENANT_CONNECTOR_TOKEN (or COVENANT_OPERATOR_TOKEN)
//         to drive a real covenantd. Needs COVENANT_LIVE_TRACE=1 on the daemon for the
//         live SSE trace.

import { Connector } from '../connector.js';
import { StubTransport, type OutboundFrame } from '../transport.js';
import { HttpDaemonClient, type AgentEvent, type DaemonClient } from '../daemon.js';
import { ScriptedDaemon, DEMO_CAPS, DEMO_TRACE } from './scenario.js';

// Mock mesh: records what the connector sends up and resolves once a terminal frame lands.
class DemoMesh extends StubTransport {
  private resolve?: (f: OutboundFrame) => void;
  done(): Promise<OutboundFrame> {
    return new Promise((r) => (this.resolve = r));
  }
  override async send(frame: OutboundFrame): Promise<void> {
    await super.send(frame);
    if (frame.type === 'result' || frame.type === 'error') this.resolve?.(frame);
  }
}

function summarize(e: AgentEvent): string {
  if (e.type === 'tool_call') return `${(e as { tool: string }).tool} — ${(e as { preview: string }).preview}`;
  if (e.type === 'tool_result') return `${(e as { tool: string }).tool} (${(e as { duration_ms: number }).duration_ms}ms${(e as { error: boolean }).error ? ', error' : ''})`;
  if (e.type === 'file_write') return `${(e as { path: string }).path} (${(e as { bytes: number }).bytes}b)`;
  return JSON.stringify(e);
}

function makeDaemon(): { daemon: DaemonClient; live: boolean } {
  const url = process.env.COVENANT_DAEMON_URL;
  const token = process.env.COVENANT_CONNECTOR_TOKEN ?? process.env.COVENANT_OPERATOR_TOKEN;
  if (url && token) return { daemon: new HttpDaemonClient(url.replace(/\/+$/, ''), token), live: true };
  return { daemon: new ScriptedDaemon(DEMO_TRACE), live: false };
}

async function main(): Promise<void> {
  const { daemon, live } = makeDaemon();
  const mesh = new DemoMesh();
  const connector = new Connector(daemon, mesh, {
    maxConcurrentDispatch: 2,
    defaultDeadlineMs: 1_800_000,
    manifestCapabilities: DEMO_CAPS,
    log: (m, e) => console.error(`[connector] ${m}${e ? ' ' + JSON.stringify(e) : ''}`),
  });
  await connector.start();

  const text =
    process.env.DEMO_INTENT ?? 'Inspect this repo, run the test suite, and write REPORT.md summarizing failures.';
  console.log(`\n── hatcher-connector demo · mode: ${live ? 'LIVE (real covenantd)' : 'OFFLINE (scripted daemon)'} ──`);
  console.log(`── dispatch: ${text}\n`);

  const done = mesh.done();
  mesh.inject({
    v: 1,
    type: 'dispatch',
    dispatch_id: 'demo-1',
    agent_id: 'HATCHER_AGENT_PK',
    intent: { text },
    deadline_ms: Date.now() + 600_000,
  });
  const final = await done;

  for (const f of mesh.sent) {
    if (f.type === 'accepted') console.log(`  accepted   intent_id=${f.intent_id}`);
    else if (f.type === 'trace') console.log(`  trace #${f.seq}  ${f.event.type.padEnd(12)} ${summarize(f.event)}`);
    else if (f.type === 'error') console.log(`  error      ${f.code}: ${f.message}`);
  }

  if (final.type === 'result') {
    console.log('\n── proof envelope (covenant.connector-trace.v0) ──');
    console.log(JSON.stringify(final.proof, null, 2));
    const ok = final.status === 'ok';
    console.log(`\n${ok ? '✓' : '✗'} dispatch ${final.status}\n`);
    process.exit(ok ? 0 : 1);
  } else if (final.type === 'error') {
    console.log(`\n✗ dispatch failed: ${final.code} — ${final.message}\n`);
    process.exit(1);
  }
}

main().catch((err) => {
  console.error('demo fatal:', err);
  process.exit(1);
});
