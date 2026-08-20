import { afterEach, describe, expect, it, vi } from 'vitest';
import { mkdirSync, mkdtempSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { callDaemonTool } from '../daemon.js';

function makeHome(): string {
  const home = mkdtempSync(path.join(tmpdir(), 'covenant-cache-'));
  mkdirSync(path.join(home, 'peers'), { recursive: true });
  return home;
}

function tokenFile(home: string): string {
  return path.join(home, 'peers', 'operator.token');
}

function recordingFetch(seen: string[]): typeof fetch {
  return (async (_input: RequestInfo | URL, init?: RequestInit) => {
    const headers = init?.headers as Record<string, string> | undefined;
    seen.push(headers?.Authorization ?? '(none)');
    return new Response(JSON.stringify({ ok: true }), {
      status: 200,
      headers: { 'content-type': 'application/json' },
    });
  }) as typeof fetch;
}

afterEach(() => {
  vi.useRealTimers();
});

describe('authToken per-path 60s cache TTL', () => {
  it('serves the cached token within the TTL and ignores an in-flight on-disk rotation', async () => {
    const home = makeHome();
    writeFileSync(tokenFile(home), 'alpha');
    const seen: string[] = [];
    const fetchImpl = recordingFetch(seen);

    await callDaemonTool('daemon_tools_list', {}, { env: { COVENANT_HOME: home }, fetchImpl });
    writeFileSync(tokenFile(home), 'beta');
    await callDaemonTool('daemon_tools_list', {}, { env: { COVENANT_HOME: home }, fetchImpl });

    expect(seen).toEqual(['Bearer alpha', 'Bearer alpha']);
  });

  it('re-reads and serves the rotated token after the TTL elapses', async () => {
    vi.useFakeTimers();
    const home = makeHome();
    writeFileSync(tokenFile(home), 'alpha');
    const seen: string[] = [];
    const fetchImpl = recordingFetch(seen);

    await callDaemonTool('daemon_tools_list', {}, { env: { COVENANT_HOME: home }, fetchImpl });
    vi.advanceTimersByTime(60_001);
    writeFileSync(tokenFile(home), 'beta');
    await callDaemonTool('daemon_tools_list', {}, { env: { COVENANT_HOME: home }, fetchImpl });

    expect(seen).toEqual(['Bearer alpha', 'Bearer beta']);
  });

  it('does not cache an empty on-disk token and serves a later write fresh', async () => {
    const home = makeHome();
    writeFileSync(tokenFile(home), '');
    const seen: string[] = [];
    const fetchImpl = recordingFetch(seen);

    const first = await callDaemonTool('daemon_tools_list', {}, { env: { COVENANT_HOME: home }, fetchImpl });
    expect(first?.isError).toBe(true);
    expect(first?.content[0]?.text).toContain('no token was readable');

    writeFileSync(tokenFile(home), 'gamma');
    const second = await callDaemonTool('daemon_tools_list', {}, { env: { COVENANT_HOME: home }, fetchImpl });
    expect(second?.isError).toBeUndefined();
    expect(seen).toEqual(['Bearer gamma']);
  });
});
