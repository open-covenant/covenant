import { createHash } from 'node:crypto';
import { mkdtempSync, readFileSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { afterEach, describe, expect, it } from 'vitest';
import { ArtifactStore, artifactConfigFromEnv } from '../src/artifacts.js';

const directories: string[] = [];
afterEach(() => {
  for (const directory of directories.splice(0))
    rmSync(directory, { recursive: true, force: true });
});

function store(overrides: Partial<ArtifactStore['config']> = {}) {
  const dir = mkdtempSync(join(tmpdir(), 'artifacts-'));
  directories.push(dir);
  return new ArtifactStore({
    dir,
    publicBaseUrl: 'https://connector.example/artifacts',
    ttlMs: 60_000,
    maxTotalBytes: 1_000,
    ...overrides,
  });
}

describe('ArtifactStore', () => {
  it('stores bytes behind an unguessable token and serves them until expiry', () => {
    const artifacts = store();
    const bytes = Buffer.from('embedding bytes');
    const meta = artifacts.save('embeddings.npy', bytes, 1_000);
    expect(meta.token).toMatch(/^[A-Za-z0-9_-]{32}$/);
    expect(meta.sha256).toBe(createHash('sha256').update(bytes).digest('hex'));
    expect(artifacts.url(meta)).toBe(
      `https://connector.example/artifacts/${meta.token}/embeddings.npy`,
    );
    const found = artifacts.open(meta.token, 'embeddings.npy', 2_000)!;
    expect(readFileSync(found.file)).toEqual(bytes);
    expect(artifacts.open(meta.token, 'other.npy', 2_000)).toBeNull();
    expect(artifacts.open('A'.repeat(32), 'embeddings.npy', 2_000)).toBeNull();
    expect(artifacts.open(meta.token, 'embeddings.npy', 61_000)).toBeNull();
  });

  it('prunes expired results and refuses to exceed its disk budget', () => {
    const artifacts = store();
    const old = artifacts.save('a.bin', Buffer.alloc(600), 1_000);
    expect(() => artifacts.save('b.bin', Buffer.alloc(600), 2_000)).toThrow(/storage is full/);
    // Expiry frees the space for the next result.
    const fresh = artifacts.save('b.bin', Buffer.alloc(600), 70_000);
    expect(artifacts.open(old.token, 'a.bin', 70_000)).toBeNull();
    expect(artifacts.open(fresh.token, 'b.bin', 70_000)).not.toBeNull();
  });

  it('returns no public link when none is configured', () => {
    const artifacts = store({ publicBaseUrl: null });
    expect(artifacts.url(artifacts.save('r.json', Buffer.from('{}')))).toBeNull();
  });

  it.each(['../x', '.hidden', 'a/b', ''])('rejects the stored name %j', (name) => {
    expect(() => store().save(name, Buffer.from('x'))).toThrow(/invalid artifact name/);
  });
});

describe('artifact configuration', () => {
  it('keeps artifacts beside the durable run store by default', () => {
    expect(artifactConfigFromEnv({ RUN_STORE_PATH: '/data/runs.json' })).toMatchObject({
      dir: '/data/artifacts',
      publicBaseUrl: null,
      ttlMs: 168 * 3_600_000,
    });
  });

  it('normalizes the public base URL', () => {
    expect(
      artifactConfigFromEnv({
        COMPUTE_ARTIFACT_BASE_URL: 'https://connector.example/artifacts/',
        COMPUTE_ARTIFACT_TTL_HOURS: '24',
      }),
    ).toMatchObject({ publicBaseUrl: 'https://connector.example/artifacts', ttlMs: 86_400_000 });
  });

  it.each([
    'http://connector.example/artifacts',
    'https://user:pw@connector.example/artifacts',
    'https://connector.example/artifacts?x=1',
  ])('rejects the public base URL %s', (value) => {
    expect(() => artifactConfigFromEnv({ COMPUTE_ARTIFACT_BASE_URL: value })).toThrow();
  });
});
