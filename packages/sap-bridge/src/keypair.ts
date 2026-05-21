// Keypair loading for the bridge worker. Reads a standard Solana CLI
// keypair file (a JSON array of 64 bytes). Kept in its own module so
// the public surface in index.ts never imports @solana/web3.js eagerly.

import type { SapKeypair } from './index.js';

export async function loadKeypairFromFile(path: string): Promise<SapKeypair> {
  const { readFileSync } = await import('node:fs');
  const { createRequire } = await import('node:module');
  // Same CJS resolver as loadSdk, so the Keypair we mint is the exact
  // class the SDK does instanceof checks against.
  const require = createRequire(import.meta.url);
  const { Keypair } = require('@solana/web3.js') as typeof import('@solana/web3.js');
  let parsed: unknown;
  try {
    parsed = JSON.parse(readFileSync(path, 'utf8').trim());
  } catch (cause) {
    throw new Error(`synapse bridge: cannot read keypair file at ${path}`, { cause });
  }
  if (!Array.isArray(parsed) || parsed.some((b) => typeof b !== 'number')) {
    throw new Error(`synapse bridge: keypair file ${path} is not a JSON byte array`);
  }
  return Keypair.fromSecretKey(Uint8Array.from(parsed as number[])) as unknown as SapKeypair;
}
