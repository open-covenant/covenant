import type { SolanaAddress } from '../solana/accounts.js';

export interface DiscoveryEventRecord {
  chain: 'solana';
  cluster: string;
  program_id: SolanaAddress;
  slot: number;
  signature: string;
  event_name: string;
  payload: Record<string, string | number | boolean>;
}

export interface DiscoveryStats {
  agents: number;
  tasks: number;
  credits_burned: string;
  stake_amount: string;
  receipt_batches: number;
  last_24h_fees: string;
}
