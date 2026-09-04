/** Package version, read once from the installed package.json. */

import { readFileSync } from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

let cached: string | undefined;

/** The version in package.json, or `0.0.0` when it cannot be read. */
export function version(): string {
  if (cached !== undefined) return cached;
  const here = path.dirname(fileURLToPath(import.meta.url));
  for (const candidate of ['../../package.json', '../../../package.json']) {
    try {
      const pkg = JSON.parse(readFileSync(path.resolve(here, candidate), 'utf8')) as {
        name?: string;
        version?: string;
      };
      if (pkg.name === '@covenant-org/desk' && pkg.version) {
        cached = pkg.version;
        return cached;
      }
    } catch {
      // Try the next layout.
    }
  }
  cached = '0.0.0';
  return cached;
}
