import assert from 'node:assert/strict';
import { test } from 'node:test';
import {
  assessRepository,
  isRepositoryName,
  MaintenanceLookupError,
  SUPPORTED_MANIFESTS,
  validationCommandFor,
} from './maintenance.js';

const NOW = () => new Date('2026-09-01T12:00:00.000Z');

function githubStub(responses: Record<string, { status?: number; body?: unknown }>) {
  const original = globalThis.fetch;
  globalThis.fetch = (async (input: string | URL | Request) => {
    const path = new URL(String(input)).pathname;
    const match = responses[path];
    if (!match) throw new Error(`unexpected request: ${path}`);
    return new Response(JSON.stringify(match.body ?? {}), {
      status: match.status ?? 200,
      headers: { 'content-type': 'application/json' },
    });
  }) as typeof fetch;
  return () => {
    globalThis.fetch = original;
  };
}

test('detects the validation command implied by each supported manifest', () => {
  assert.deepEqual(validationCommandFor(['pnpm-lock.yaml']), ['pnpm-lock.yaml', 'pnpm test']);
  assert.deepEqual(validationCommandFor(['Cargo.toml']), ['Cargo.toml', 'cargo test']);
  assert.deepEqual(validationCommandFor(['go.mod']), ['go.mod', 'go test ./...']);
  assert.equal(validationCommandFor(['README.md']), undefined);
});

test('reports a qualifying repository with the command Mizuki would run', async () => {
  const restore = githubStub({
    '/repos/example/project': { body: { private: false, default_branch: 'main' } },
    '/repos/example/project/contents/': {
      body: [
        { name: 'pnpm-lock.yaml', type: 'file' },
        { name: 'src', type: 'dir' },
      ],
    },
  });
  try {
    const assessment = await assessRepository('example', 'project', { now: NOW });
    assert.equal(assessment.eligible, true);
    assert.equal(assessment.validationCommand, 'pnpm test');
    assert.equal(assessment.detectedManifest, 'pnpm-lock.yaml');
    assert.equal(assessment.defaultBranch, 'main');
    assert.equal(assessment.repository, 'example/project');
  } finally {
    restore();
  }
});

test('names the manifests that would qualify a repository that has none', async () => {
  const restore = githubStub({
    '/repos/example/docs': { body: { private: false, default_branch: 'main' } },
    '/repos/example/docs/contents/': { body: [{ name: 'README.md', type: 'file' }] },
  });
  try {
    const assessment = await assessRepository('example', 'docs', { now: NOW });
    assert.equal(assessment.eligible, false);
    for (const manifest of SUPPORTED_MANIFESTS) {
      assert.ok(assessment.reason?.includes(manifest), `reason should name ${manifest}`);
    }
  } finally {
    restore();
  }
});

test('refuses a private repository without reading its contents', async () => {
  const restore = githubStub({
    '/repos/example/secret': { body: { private: true, default_branch: 'main' } },
  });
  try {
    const assessment = await assessRepository('example', 'secret', { now: NOW });
    assert.equal(assessment.eligible, false);
    assert.match(assessment.reason ?? '', /public repositories only/);
  } finally {
    restore();
  }
});

test('reports an archived repository as ineligible', async () => {
  const restore = githubStub({
    '/repos/example/old': { body: { private: false, archived: true, default_branch: 'main' } },
  });
  try {
    const assessment = await assessRepository('example', 'old', { now: NOW });
    assert.equal(assessment.eligible, false);
    assert.match(assessment.reason ?? '', /archived/);
  } finally {
    restore();
  }
});

test('separates a missing repository from an unavailable GitHub', async () => {
  const missing = githubStub({ '/repos/example/nope': { status: 404 } });
  try {
    await assert.rejects(
      () => assessRepository('example', 'nope', { now: NOW }),
      (error: unknown) => error instanceof MaintenanceLookupError && error.status === 404,
    );
  } finally {
    missing();
  }

  const limited = githubStub({ '/repos/example/busy': { status: 403 } });
  try {
    await assert.rejects(
      () => assessRepository('example', 'busy', { now: NOW }),
      (error: unknown) => error instanceof MaintenanceLookupError && error.status === 503,
    );
  } finally {
    limited();
  }
});

test('rejects path segments that are not GitHub names', () => {
  assert.equal(isRepositoryName('covenant'), true);
  assert.equal(isRepositoryName('open-covenant'), true);
  assert.equal(isRepositoryName('..'), false);
  assert.equal(isRepositoryName('a/b'), false);
  assert.equal(isRepositoryName(''), false);
});
