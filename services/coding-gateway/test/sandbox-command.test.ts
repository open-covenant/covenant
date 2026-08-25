import { execFile } from 'node:child_process';
import { mkdtemp, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { promisify } from 'node:util';
import { afterEach, describe, expect, it } from 'vitest';
import { isolatedShellCommand } from '../src/sandbox-command.js';

const run = promisify(execFile);
const directories: string[] = [];

afterEach(async () => {
  await Promise.all(
    directories.splice(0).map((directory) => rm(directory, { recursive: true, force: true })),
  );
});

describe('isolatedShellCommand', () => {
  it('keeps package-manager state outside the repository and preserves shell quoting', async () => {
    const repository = await mkdtemp(join(tmpdir(), 'mizuki-shell-'));
    directories.push(repository);
    const command = [
      `printf '%s\\n' "$HOME"`,
      `printf '%s\\n' "$XDG_CACHE_HOME"`,
      `printf '%s\\n' "$NPM_CONFIG_CACHE"`,
      `printf '%s\\n' "$COREPACK_HOME"`,
      `printf '%s\\n' "$PNPM_HOME"`,
      `printf '%s\\n' "$YARN_CACHE_FOLDER"`,
      `printf '%s\\n' "maintainer's exact head"`,
    ].join('; ');

    const { stdout } = await run('/bin/bash', ['-lc', isolatedShellCommand(command)], {
      cwd: repository,
    });
    const lines = stdout.trimEnd().split('\n');
    const stateDirectories = lines.slice(0, 6).map((directory) => resolve(directory));

    expect(stateDirectories).toHaveLength(6);
    expect(
      stateDirectories.every((directory) => !directory.startsWith(`${resolve(repository)}/`)),
    ).toBe(true);
    expect(lines[6]).toBe("maintainer's exact head");
  });
});
