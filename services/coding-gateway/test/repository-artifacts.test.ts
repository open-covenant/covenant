import { describe, expect, it } from 'vitest';
import { captureRepositoryFiles } from '../src/repository-artifacts.js';

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
});
