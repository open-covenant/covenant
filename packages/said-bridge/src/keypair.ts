import type { Keypair } from '@solana/web3.js';

const SECRET_KEY_BYTES = 64;

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

  if (!Array.isArray(parsed)) {
    throw new Error(`said bridge: keypair file ${path} is not a JSON array`);
  }
  if (parsed.length !== SECRET_KEY_BYTES) {
    throw new Error(
      `said bridge: keypair file ${path} has ${parsed.length} bytes, expected ${SECRET_KEY_BYTES}`,
    );
  }
  if (!parsed.every((b) => Number.isInteger(b) && b >= 0 && b <= 255)) {
    throw new Error(`said bridge: keypair file ${path} has out-of-range bytes`);
  }

  try {
    return web3.Keypair.fromSecretKey(Uint8Array.from(parsed as number[]));
  } catch (cause) {
    throw new Error(
      `said bridge: keypair file ${path} is not a valid ed25519 secret key`,
      { cause },
    );
  }
}
