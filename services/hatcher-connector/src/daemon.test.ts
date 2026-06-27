import { afterEach, describe, expect, it, vi } from 'vitest';
import { HttpDaemonClient, DaemonUnreachableError } from './daemon.js';

function client(retries = 1): HttpDaemonClient {
  return new HttpDaemonClient('http://daemon.local', 'operator-token', 1000, retries);
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
