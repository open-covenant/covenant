import { afterEach, describe, expect, it, vi } from 'vitest';
import { HttpDaemonClient, DaemonUnreachableError, type AgentEvent } from './daemon.js';

function client(retries = 1): HttpDaemonClient {
  return new HttpDaemonClient('http://daemon.local', 'operator-token', 1000, retries);
}

function jsonResponse(body: unknown): Response {
  return new Response(JSON.stringify(body), {
    status: 200,
    headers: { 'content-type': 'application/json' },
  });
}

async function rejection(p: Promise<unknown>): Promise<Error> {
  try {
    await p;
  } catch (err) {
    return err as Error;
  }
  throw new Error('expected the call to reject');
}

afterEach(() => {
  vi.unstubAllGlobals();
});

// HttpDaemonClient.fetchJson classifies failures into a daemon answer
// (DaemonHttpError, status preserved) versus a transport outage
// (DaemonUnreachableError). The connector leans on that split to report a policy
// denial as a denial and a down daemon as `daemon_unreachable` rather than
// conflating the two. connector.test.ts only ever exercises a FakeDaemon, so the
// real classifier reached through the public intentResult() path is otherwise
// uncovered.
describe('HttpDaemonClient fetchJson error classification', () => {
  it('does not retry a 4xx and surfaces it as a daemon error, never as unreachable', async () => {
    const fetchMock = vi.fn<typeof fetch>(async () => new Response(null, { status: 403 }));
    vi.stubGlobal('fetch', fetchMock);

    const err = await rejection(client().intentResult('x'));

    expect(err).not.toBeInstanceOf(DaemonUnreachableError);
    expect(err.message).toContain('-> 403');
    // status < 500 is a daemon answer, not a transient fault: one attempt only.
    expect(fetchMock).toHaveBeenCalledTimes(1);
  });

  it('retries a 5xx daemon error before surfacing it', async () => {
    const fetchMock = vi.fn<typeof fetch>(async () => new Response(null, { status: 503 }));
    vi.stubGlobal('fetch', fetchMock);

    const err = await rejection(client().intentResult('x'));

    expect(err).not.toBeInstanceOf(DaemonUnreachableError);
    expect(err.message).toContain('-> 503');
    // retries=1: one retry after the initial attempt.
    expect(fetchMock).toHaveBeenCalledTimes(2);
  });

  it('classifies a transport failure as DaemonUnreachableError, not a policy denial', async () => {
    const fetchMock = vi.fn<typeof fetch>(async () => {
      throw new TypeError('ECONNREFUSED');
    });
    vi.stubGlobal('fetch', fetchMock);

    const err = await rejection(client().intentResult('x'));

    expect(err).toBeInstanceOf(DaemonUnreachableError);
    expect(err.message).toContain('/intents/x/result');
    // A thrown fetch is transient: retried, then surfaced as unreachable.
    expect(fetchMock).toHaveBeenCalledTimes(2);
  });
});

// signAttestation and verify read back security-bearing daemon responses — an
// ed25519 identity attestation and the audit-integrity report. Each rejects a
// daemon error and an incomplete body so the connector never pairs on a bogus
// attestation or treats a missing integrity report as a clean verify.
describe('HttpDaemonClient response guards', () => {
  it('signAttestation surfaces a daemon error', async () => {
    vi.stubGlobal('fetch', vi.fn(async () => jsonResponse({ kind: 'error', message: 'no identity.attest cap' })));

    await expect(client().signAttestation('msg', 7)).rejects.toThrow('daemon attestation error: no identity.attest cap');
  });

  it('signAttestation rejects a body missing the signature or pubkey', async () => {
    vi.stubGlobal('fetch', vi.fn(async () => jsonResponse({ ts: 7 })));

    await expect(client().signAttestation('msg', 7)).rejects.toThrow('unexpected /identity/sign response');
  });

  it('signAttestation returns the attestation for a complete body', async () => {
    vi.stubGlobal('fetch', vi.fn(async () => jsonResponse({ signature_b58: 'sig', pubkey_b58: 'pk', ts: 5 })));

    await expect(client().signAttestation('msg', 7)).resolves.toEqual({ signature_b58: 'sig', pubkey_b58: 'pk', ts: 5 });
  });

  it('verify surfaces a daemon error', async () => {
    vi.stubGlobal('fetch', vi.fn(async () => jsonResponse({ kind: 'error', message: 'audit chain broken' })));

    await expect(client().verify()).rejects.toThrow('daemon verify error: audit chain broken');
  });

  it('verify rejects a response with no report', async () => {
    vi.stubGlobal('fetch', vi.fn(async () => jsonResponse({ kind: 'verify_report' })));

    await expect(client().verify()).rejects.toThrow('daemon verify returned no report');
  });

  it('verify returns the report when present', async () => {
    const report = { events: 3, anchors: 1, valid: true, root_hash_hex: 'ab', failures: [] };
    vi.stubGlobal('fetch', vi.fn(async () => jsonResponse({ report })));

    await expect(client().verify()).resolves.toEqual(report);
  });
});

// submitIntent is the primary connector entrypoint (Hatcher dispatch -> covenant
// intent). It must surface a daemon-side rejection rather than report it as
// accepted, refuse a malformed /intent response, and default the optional
// text/sources fields. connector.test.ts only drives a FakeDaemon, so the real
// mapping is otherwise uncovered.
describe('HttpDaemonClient submitIntent', () => {
  it('surfaces the daemon error message on a rejected intent', async () => {
    vi.stubGlobal('fetch', vi.fn(async () => jsonResponse({ kind: 'error', message: 'policy denied' })));

    await expect(client().submitIntent('do a thing')).rejects.toThrow('daemon intent error: policy denied');
  });

  it('rejects a malformed /intent response shape', async () => {
    vi.stubGlobal('fetch', vi.fn(async () => jsonResponse({ intent_id: 7, status: 'queued' })));
    await expect(client().submitIntent('x')).rejects.toThrow('unexpected /intent response shape');

    vi.stubGlobal('fetch', vi.fn(async () => jsonResponse({ intent_id: 'i1' })));
    await expect(client().submitIntent('x')).rejects.toThrow('unexpected /intent response shape');
  });

  it('returns a typed IntentResult, defaulting optional text/sources', async () => {
    vi.stubGlobal('fetch', vi.fn(async () => jsonResponse({ intent_id: 'i1', status: 'queued' })));

    await expect(client().submitIntent('x')).resolves.toEqual({
      intent_id: 'i1',
      status: 'queued',
      text: '',
      sources: [],
    });
  });

  it('passes through a fully-populated IntentResult', async () => {
    const full = { intent_id: 'i2', status: 'ok', text: 'done', sources: ['mem://a'] };
    vi.stubGlobal('fetch', vi.fn(async () => jsonResponse(full)));

    await expect(client().submitIntent('x')).resolves.toEqual(full);
  });
});

// streamEvents parses the daemon's SSE trace stream and forwards only well-formed
// stream_chunk frames to the mesh. It must skip keepalive comments, ignore
// stream_begin/stream_end and malformed frames, and reassemble a frame split
// across read() boundaries. Uncovered: connector.test.ts drives a FakeDaemon.
describe('HttpDaemonClient streamEvents', () => {
  function frame(obj: unknown): string {
    return `data: ${JSON.stringify(obj)}\n\n`;
  }
  function streamFrom(parts: string[]): Response {
    const enc = new TextEncoder();
    const body = new ReadableStream({
      start(c) {
        for (const p of parts) c.enqueue(enc.encode(p));
        c.close();
      },
    });
    return new Response(body, { status: 200 });
  }
  async function collect(parts: string[]): Promise<AgentEvent[]> {
    vi.stubGlobal('fetch', vi.fn(async () => streamFrom(parts)));
    const events: AgentEvent[] = [];
    await client().streamEvents('i1', (e) => events.push(e), new AbortController().signal);
    return events;
  }

  it('forwards only well-formed stream_chunk frames', async () => {
    const events = await collect([
      ': keepalive\n\n',
      frame({ kind: 'stream_begin' }),
      frame({ kind: 'stream_chunk', chunk: { type: 'tool_use', name: 'x' } }),
      frame({ kind: 'stream_chunk' }),
      frame({ kind: 'stream_chunk', chunk: { text: 'no type' } }),
      frame({ kind: 'control', chunk: { type: 'sneaky' } }),
      'data: not-json\n\n',
      frame({ kind: 'stream_chunk', chunk: { type: 'text', text: 'hi' } }),
      frame({ kind: 'stream_end' }),
    ]);
    expect(events).toEqual([
      { type: 'tool_use', name: 'x' },
      { type: 'text', text: 'hi' },
    ]);
  });

  it('reassembles a stream_chunk frame split across read boundaries', async () => {
    const events = await collect([
      'data: {"kind":"stream_chunk","chunk":{"type":"te',
      'xt","text":"hi"}}\n\n',
    ]);
    expect(events).toEqual([{ type: 'text', text: 'hi' }]);
  });

  it('throws when the events endpoint does not return ok', async () => {
    // Non-null body so the rejection is decided by !res.ok, not the null-body arm:
    // dropping the ok guard would otherwise read this as an (empty) event stream.
    vi.stubGlobal('fetch', vi.fn(async () => new Response('upstream error', { status: 503 })));

    await expect(
      client().streamEvents('i1', () => {}, new AbortController().signal),
    ).rejects.toThrow('/intents/i1/events');
  });
});
