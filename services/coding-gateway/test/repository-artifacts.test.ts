import { describe, expect, it } from 'vitest';
import { assertRepositoryPatchSize, captureRepositoryFiles } from '../src/repository-artifacts.js';

function sandbox(files: Record<string, string>) {
  return {
    async readFile(path: string) {
      const content = files[path];
      if (content === undefined) throw new Error('not found');
      return content;
    },
  };
}

describe('repository artifact capture', () => {
  it('returns changed file bytes exactly instead of truncating them', async () => {
    const content = 'a'.repeat(64 * 1024 + 1);

    await expect(
      captureRepositoryFiles(sandbox({ 'large.txt': content }), ['large.txt']),
    ).resolves.toEqual([{ path: 'large.txt', content }]);
  });

  it('rejects a changed file above the publication limit', async () => {
    const content = 'a'.repeat(128_001);

    await expect(
      captureRepositoryFiles(sandbox({ 'too-large.txt': content }), ['too-large.txt']),
    ).rejects.toThrow('changed file exceeds the 128000-byte capture limit: too-large.txt');
  });

  it('rejects unavailable and binary changed files', async () => {
    await expect(captureRepositoryFiles(sandbox({}), ['missing.txt'])).rejects.toThrow(
      'changed file is unavailable: missing.txt',
    );
    await expect(
      captureRepositoryFiles(sandbox({ 'binary.dat': 'prefix\u0000suffix' }), ['binary.dat']),
    ).rejects.toThrow('binary changed file is unsupported: binary.dat');
  });

  it('rejects more than 40 changed files before reading any of them', async () => {
    let reads = 0;
    const paths = Array.from({ length: 41 }, (_, index) => `file-${index}.txt`);
    const source = {
      async readFile() {
        reads++;
        return 'content';
      },
    };

    await expect(captureRepositoryFiles(source, paths)).rejects.toThrow(
      'repository change exceeds the 40-file capture limit',
    );
    expect(reads).toBe(0);
  });

  it('rejects a patch above the review and persistence limit', () => {
    expect(() => assertRepositoryPatchSize('a'.repeat(1_000_001))).toThrow(
      'repository patch exceeds the 1000000-byte limit',
    );
  });
});
