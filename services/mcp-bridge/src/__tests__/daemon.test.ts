import { describe, expect, it } from 'vitest';
import { mkdtempSync, mkdirSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { callDaemonTool } from '../daemon.js';
import { tools } from '../server.js';

function fetchJson(
  handler: (url: URL, init: RequestInit) => { status?: number; body?: unknown },
): typeof fetch {
  return (async (input: RequestInfo | URL, init?: RequestInit) => {
    const url = input instanceof URL ? input : new URL(String(input));
    const result = handler(url, init ?? {});
    return new Response(JSON.stringify(result.body ?? { ok: true }), {
      status: result.status ?? 200,
      headers: { 'content-type': 'application/json' },
    });
  }) as typeof fetch;
}

describe('daemon-backed MCP tools', () => {
  it('lists Solana prep tools and daemon tools together', () => {
    const names = tools.map((tool) => tool.name);
    expect(names).toContain('prepare_register_agent');
    expect(names).toContain('prepare_anchor_receipt_batch');
    expect(names).toContain('daemon_tools_list');
    expect(names).toContain('daemon_submit_intent');
  });

  it('sends daemon requests with bearer auth', async () => {
    const fetchImpl = fetchJson((url, init) => {
      expect(url.toString()).toBe('http://127.0.0.1:8421/tools');
      expect(init.headers).toMatchObject({ Authorization: 'Bearer test-token' });
      return { body: { tools: [{ name: 'echo' }] } };
    });

    const result = await callDaemonTool('daemon_tools_list', {}, {
      env: { COVENANT_AUTH_TOKEN: 'test-token' },
      fetchImpl,
    });

    expect(result?.isError).toBeUndefined();
    expect(result?.content[0]?.text).toContain('"echo"');
  });

  it('rejects missing daemon tokens without leaking local paths', async () => {
    const home = mkdtempSync(path.join(tmpdir(), 'covenant-home-'));
    let called = false;
    const fetchImpl = fetchJson(() => {
      called = true;
      return {};
    });

    const result = await callDaemonTool('daemon_tools_list', {}, {
      env: { HOME: home },
      fetchImpl,
    });

    expect(called).toBe(false);
    expect(result?.isError).toBe(true);
    expect(result?.content[0]?.text).toContain('$HOME/.covenant/peers/operator.token');
    expect(result?.content[0]?.text).not.toContain(home);
  });

  it('redacts bearer tokens and local paths from daemon errors', async () => {
    const home = mkdtempSync(path.join(tmpdir(), 'covenant-home-'));
    const token = 'secret-bearer-token';
    const fetchImpl = fetchJson(() => ({
      status: 500,
      body: { error: `token ${token} failed under ${home}` },
    }));

    const result = await callDaemonTool('daemon_tools_list', {}, {
      env: { COVENANT_AUTH_TOKEN: token, HOME: home },
      fetchImpl,
    });

    expect(result?.isError).toBe(true);
    expect(result?.content[0]?.text).toContain('<redacted-token>');
    expect(result?.content[0]?.text).toContain('$HOME');
    expect(result?.content[0]?.text).not.toContain(token);
    expect(result?.content[0]?.text).not.toContain(home);
  });

  it('redacts a COVENANT_HOME directory value from daemon errors', async () => {
    const home = mkdtempSync(path.join(tmpdir(), 'covenant-home-'));
    const token = 'secret-bearer-token';
    const fetchImpl = fetchJson(() => ({
      status: 500,
      body: { error: `token ${token} under ${home} failed` },
    }));

    const result = await callDaemonTool('daemon_tools_list', {}, {
      env: { COVENANT_AUTH_TOKEN: token, COVENANT_HOME: home },
      fetchImpl,
    });

    expect(result?.isError).toBe(true);
    expect(result?.content[0]?.text).toContain('<redacted-token>');
    expect(result?.content[0]?.text).toContain('$HOME');
    expect(result?.content[0]?.text).not.toContain(token);
    expect(result?.content[0]?.text).not.toContain(home);
  });

  it('references the $COVENANT_HOME token label when COVENANT_HOME is set', async () => {
    const home = mkdtempSync(path.join(tmpdir(), 'covenant-home-'));
    let called = false;
    const fetchImpl = fetchJson(() => {
      called = true;
      return {};
    });

    const result = await callDaemonTool('daemon_tools_list', {}, {
      env: { COVENANT_HOME: home },
      fetchImpl,
    });

    expect(called).toBe(false);
    expect(result?.isError).toBe(true);
    expect(result?.content[0]?.text).toContain('$COVENANT_HOME/peers/operator.token');
    expect(result?.content[0]?.text).not.toContain(home);
  });

  it('reads the operator token from COVENANT_HOME when set', async () => {
    const home = mkdtempSync(path.join(tmpdir(), 'covenant-home-'));
    mkdirSync(path.join(home, 'peers'), { recursive: true });
    writeFileSync(path.join(home, 'peers', 'operator.token'), 'on-disk-token');

    const fetchImpl = fetchJson((_url, init) => {
      expect(init.headers).toMatchObject({ Authorization: 'Bearer on-disk-token' });
      return { body: { tools: [{ name: 'echo' }] } };
    });

    const result = await callDaemonTool('daemon_tools_list', {}, {
      env: { COVENANT_HOME: home },
      fetchImpl,
    });

    expect(result?.isError).toBeUndefined();
    expect(result?.content[0]?.text).toContain('"echo"');
  });
});
