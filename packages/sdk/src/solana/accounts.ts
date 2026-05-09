import { SOLANA_ADDRESS_REGEX } from '@covenant/config/networks';

export type SolanaAddress = string;
export type Hash32 = string;

export function isSolanaAddress(value: string): value is SolanaAddress {
  return SOLANA_ADDRESS_REGEX.test(value);
}

export function assertSolanaAddress(value: string, label = 'address'): SolanaAddress {
  if (!isSolanaAddress(value)) {
    throw new Error(`${label} must be a Solana base58 address`);
  }
  return value;
}

export function assertHash32(value: string, label = 'hash'): Hash32 {
  if (!/^[a-f0-9]{64}$/i.test(value)) {
    throw new Error(`${label} must be a 32-byte lowercase-or-uppercase hex string`);
  }
  return value.toLowerCase();
}

export const ACCOUNT_SEEDS = Object.freeze({
  config: 'config',
  agent: 'agent',
  credits: 'credits',
  stake: 'stake',
  task: 'task',
  receiptBatch: 'receipt_batch',
});
