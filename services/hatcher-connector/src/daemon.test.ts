import { describe, it, expect, vi, afterEach } from 'vitest';
import { HttpDaemonClient } from './daemon.js';
import type { AgentEvent } from './daemon.js';

// Build a fetch Response whose body streams the given SSE text in arbitrary chunks.
function sseResponse(text: string, chunkAt = text.length): Response {
  const enc = new TextEncoder();
  const parts: Uint8Array[] = [];
  for (let i = 0; i < text.length; i += chunkAt) parts.push(enc.encode(text.slice(i, i + chunkAt)));
  const body = new ReadableStream<Uint8Array>({
    start(c) {
      for (const p of parts) c.enqueue(p);
      c.close();
    },
  });
  return new Response(body, { status: 200, headers: { 'content-type': 'text/event-stream' } });
}

afterEach(() => vi.restoreAllMocks());

describe('HttpDaemonClient.streamEvents', () => {
  // covenantd /intents/:id/events emits the raw AgentEvent per frame
  // (`data: {"type":...}`), NOT the v2 stream_chunk envelope. Guard the parser
  // against silently dropping every event (the trace_count=0 bug).
  it('parses raw AgentEvent SSE frames and ignores keepalive comments', async () => {
    const wire =
      ': keepalive\n\n' +
      'data: {"type":"tool_call","run_id":"r1","tool":"write_file","preview":"hello.js"}\n\n' +
      'data: {"type":"file_write","run_id":"r1","path":"hello.js","bytes":42}\n\n' +
      ': keepalive\n\n' +
      'data: {"type":"tool_result","run_id":"r1","tool":"write_file","duration_ms":3,"error":false}\n\n';
    vi.spyOn(globalThis, 'fetch').mockResolvedValue(sseResponse(wire));

    const client = new HttpDaemonClient('http://d', 'tok');
    const got: AgentEvent[] = [];
    await client.streamEvents('int-1', (e) => got.push(e), new AbortController().signal);

    expect(got.map((e) => e.type)).toEqual(['tool_call', 'file_write', 'tool_result']);
    expect((got[1] as { path: string }).path).toBe('hello.js');
  });

  it('reassembles events split across read chunks', async () => {
    const wire = 'data: {"type":"tool_call","run_id":"r1","tool":"bash","preview":"node hello.js"}\n\n';
    vi.spyOn(globalThis, 'fetch').mockResolvedValue(sseResponse(wire, 7)); // tiny chunks

    const client = new HttpDaemonClient('http://d', 'tok');
    const got: AgentEvent[] = [];
    await client.streamEvents('int-1', (e) => got.push(e), new AbortController().signal);

    expect(got).toHaveLength(1);
    expect(got[0].type).toBe('tool_call');
  });
});
