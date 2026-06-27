import { afterEach, describe, expect, it, vi } from 'vitest';
import { HttpDaemonClient, DaemonUnreachableError } from './daemon.js';

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
