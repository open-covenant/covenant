// Reads a Solana CLI keypair file (JSON array of 64 bytes). The exact
// Keypair class returned must come from the same @solana/web3.js the
// said-sdk resolved, so we use createRequire to grab the runtime module.

import type { Keypair } from '@solana/web3.js';

export async function loadKeypairFromFile(path: string): Promise<Keypair> {
  const { readFileSync } = await import('node:fs');
  const { createRequire } = await import('node:module');
  const require = createRequire(import.meta.url);
  const web3 = require('@solana/web3.js') as typeof import('@solana/web3.js');
  let parsed: unknown;
  try {
    parsed = JSON.parse(readFileSync(path, 'utf8').trim());
  } catch (cause) {
    throw new Error(`said bridge: cannot read keypair file at ${path}`, { cause });
  }
  if (!Array.isArray(parsed) || parsed.some((b) => typeof b !== 'number')) {
    throw new Error(`said bridge: keypair file ${path} is not a JSON byte array`);
  }
  return web3.Keypair.fromSecretKey(Uint8Array.from(parsed as number[]));
}
