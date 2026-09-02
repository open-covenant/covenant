import assert from 'node:assert/strict';
import { test } from 'node:test';
import { MizukiToolset, isGithubName } from './index.js';

const stub = (status: number, body: unknown, capture?: { url?: string; init?: RequestInit }) =>
  (async (url: string, init: RequestInit) => {
    if (capture) {
      capture.url = String(url);
      capture.init = init;
    }
    return new Response(JSON.stringify(body), {
      status,
      headers: { 'content-type': 'application/json' },
    });
  }) as unknown as typeof fetch;

test('quotes against the public service by default', async () => {
  const seen: { url?: string; init?: RequestInit } = {};
  const t = new MizukiToolset({ fetchImpl: stub(201, { quote_id: 'q-1' }, seen) });

  const out = await t.quote('https://github.com/open-covenant/covenant/issues/9');

  assert.match(out, /q-1/);
  assert.equal(seen.url, 'https://mizuki.opencovenant.org/api/mizuki/v1/quotes');
});

test('relays why an issue was refused instead of throwing', async () => {
  const t = new MizukiToolset({
    fetchImpl: stub(422, { error: 'Choose an open GitHub issue for paid maintenance.' }),
  });

  const out = await t.quote('https://github.com/open-covenant/covenant/pull/3');

  assert.match(out, /declined/);
  assert.match(out, /Choose an open GitHub issue/);
});

test('surfaces a payment challenge as a challenge, not an error', async () => {
  const t = new MizukiToolset({ fetchImpl: stub(402, { x402Version: 2 }) });

  assert.match(await t.assess('open-covenant', 'covenant'), /paid endpoint/);
});

test('refuses path segments that are not GitHub names', async () => {
  let called = false;
  const t = new MizukiToolset({
    fetchImpl: (async () => {
      called = true;
      return new Response('{}');
    }) as unknown as typeof fetch,
  });

  assert.match(await t.assess('..', 'covenant'), /must be GitHub names/);
  assert.equal(called, false);
  assert.equal(isGithubName('..'), false);
  assert.equal(isGithubName('open-covenant'), true);
});

test('distinguishes an unknown job from a service failure', async () => {
  const missing = new MizukiToolset({ fetchImpl: stub(404, {}) });
  assert.match(await missing.jobStatus('abc'), /No Mizuki job found/);

  const broken = new MizukiToolset({ fetchImpl: stub(500, { error: 'upstream' }) });
  assert.match(await broken.jobStatus('abc'), /Could not read that Mizuki job/);
});

test('sends a maintainer token when one is configured', async () => {
  const seen: { url?: string; init?: RequestInit } = {};
  const t = new MizukiToolset({ apiToken: 'tok', fetchImpl: stub(200, { bounties: [] }, seen) });

  await t.bounties();

  assert.equal(new Headers(seen.init?.headers).get('authorization'), 'Bearer tok');
});

test('reports an unreachable service without throwing', async () => {
  const t = new MizukiToolset({
    fetchImpl: (async () => {
      throw new Error('ECONNREFUSED');
    }) as unknown as typeof fetch,
  });

  assert.match(await t.bounties(), /Could not reach Mizuki/);
});
