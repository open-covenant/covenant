import { cpSync, existsSync, mkdirSync } from 'node:fs';
import { resolve } from 'node:path';

const output = resolve('.next/standalone/apps/mizuki-web');
if (!existsSync(output)) throw new Error('Standalone server output was not generated');

mkdirSync(resolve(output, '.next'), { recursive: true });
cpSync(resolve('.next/static'), resolve(output, '.next/static'), { recursive: true });
if (existsSync(resolve('public'))) {
  cpSync(resolve('public'), resolve(output, 'public'), { recursive: true });
}
