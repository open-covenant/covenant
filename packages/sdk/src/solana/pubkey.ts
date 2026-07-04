import { PublicKey } from '@solana/web3.js';
import type { SolanaAddress } from './accounts.js';

// Anywhere the SDK needs a pubkey it accepts either a base58 string or a
// web3.js PublicKey, so callers can stay in whichever representation they hold.
export type Address = SolanaAddress | PublicKey;

export function toPublicKey(value: Address): PublicKey {
  return value instanceof PublicKey ? value : new PublicKey(value);
}

export function toBase58(value: Address): string {
  return value instanceof PublicKey ? value.toBase58() : value;
}
