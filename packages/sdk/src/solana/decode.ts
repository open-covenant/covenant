import type { Connection } from '@solana/web3.js';
import { BorshReader } from './borsh.js';
import type { Hash32, SolanaAddress } from './accounts.js';
import { toPublicKey, type Address } from './pubkey.js';

// Decoded program state. u64/i64 come back as bigint (lossless); [u8;32] fields
// as lowercase hex (the SDK's Hash32); pubkeys as base58 strings.

export interface AgentAccount {
  agentKey: Hash32;
  operator: SolanaAddress;
  metadataHash: Hash32;
  capabilityHash: Hash32;
  stake: bigint;
  reputation: bigint;
  active: boolean;
  bump: number;
}

export interface ConfigAccount {
  authority: SolanaAddress;
  slashAuthority: SolanaAddress;
  covntMint: SolanaAddress;
  treasury: SolanaAddress;
  creditsPerCovnt: bigint;
  paused: boolean;
  bump: number;
  minStakeLock: bigint;
}

export interface CreditAccount {
  owner: SolanaAddress;
  balance: bigint;
  bump: number;
}

export interface ReceiptBatchAccount {
  batchId: Hash32;
  authority: SolanaAddress;
  merkleRoot: Hash32;
  receiptCount: number;
  createdAt: bigint;
  bump: number;
}

export interface StakePositionAccount {
  agentKey: Hash32;
  owner: SolanaAddress;
  amount: bigint;
  lockUntil: bigint;
  active: boolean;
  bump: number;
}

export interface TaskAccount {
  taskId: Hash32;
  client: SolanaAddress;
  agentKey: Hash32;
  provider: SolanaAddress;
  amountCovnt: bigint;
  taskHash: Hash32;
  criteriaHash: Hash32;
  resultHash: Hash32;
  deadline: bigint;
  status: number;
  bump: number;
}

const DISCRIMINATORS = {
  Agent: [47, 166, 112, 147, 155, 197, 86, 7],
  Config: [155, 12, 170, 224, 30, 250, 204, 130],
  CreditAccount: [196, 171, 234, 132, 239, 255, 21, 96],
  ReceiptBatch: [234, 250, 48, 59, 242, 148, 55, 76],
  StakePosition: [78, 165, 30, 111, 171, 125, 11, 220],
  Task: [79, 34, 229, 55, 88, 90, 55, 84],
} as const;

// These decoders are settlement-program only. Anchor account discriminators
// depend only on the struct name, so the settlement Config and StakePosition
// share bytes with the stake program's same-named accounts. Gating on the
// discriminator alone would let a stake-program account decode as a settlement
// struct and silently misread, so every decode also requires the buffer to be
// fully consumed: the colliding stake accounts are larger and leave a trailing
// tail, which turns a silent misread into a thrown error.
function decodeAccount<T>(
  data: Uint8Array,
  account: keyof typeof DISCRIMINATORS,
  fields: (reader: BorshReader) => T,
): T {
  const reader = new BorshReader(data);
  const found = reader.discriminator();
  const want = DISCRIMINATORS[account];
  for (let i = 0; i < 8; i++) {
    if (found[i] !== want[i]) throw new Error(`account does not match ${account} (discriminator mismatch)`);
  }
  let value: T;
  try {
    value = fields(reader);
  } catch (err) {
    throw new Error(`failed to decode ${account} (${data.length} bytes): ${(err as Error).message}`);
  }
  const trailing = reader.remaining();
  if (trailing !== 0) {
    throw new Error(`account does not match ${account}: ${trailing} trailing bytes (wrong account type or program?)`);
  }
  return value;
}

export function decodeAgent(data: Uint8Array): AgentAccount {
  return decodeAccount(data, 'Agent', (r) => ({
    agentKey: r.hash32(),
    operator: r.pubkey(),
    metadataHash: r.hash32(),
    capabilityHash: r.hash32(),
    stake: r.u64(),
    reputation: r.u64(),
    active: r.bool(),
    bump: r.u8(),
  }));
}

export function decodeConfig(data: Uint8Array): ConfigAccount {
  return decodeAccount(data, 'Config', (r) => ({
    authority: r.pubkey(),
    slashAuthority: r.pubkey(),
    covntMint: r.pubkey(),
    treasury: r.pubkey(),
    creditsPerCovnt: r.u64(),
    paused: r.bool(),
    bump: r.u8(),
    minStakeLock: r.u64(),
  }));
}

export function decodeCreditAccount(data: Uint8Array): CreditAccount {
  return decodeAccount(data, 'CreditAccount', (r) => ({ owner: r.pubkey(), balance: r.u64(), bump: r.u8() }));
}

export function decodeReceiptBatch(data: Uint8Array): ReceiptBatchAccount {
  return decodeAccount(data, 'ReceiptBatch', (r) => ({
    batchId: r.hash32(),
    authority: r.pubkey(),
    merkleRoot: r.hash32(),
    receiptCount: r.u32(),
    createdAt: r.i64(),
    bump: r.u8(),
  }));
}

export function decodeStakePosition(data: Uint8Array): StakePositionAccount {
  return decodeAccount(data, 'StakePosition', (r) => ({
    agentKey: r.hash32(),
    owner: r.pubkey(),
    amount: r.u64(),
    lockUntil: r.u64(),
    active: r.bool(),
    bump: r.u8(),
  }));
}

export function decodeTask(data: Uint8Array): TaskAccount {
  return decodeAccount(data, 'Task', (r) => ({
    taskId: r.hash32(),
    client: r.pubkey(),
    agentKey: r.hash32(),
    provider: r.pubkey(),
    amountCovnt: r.u64(),
    taskHash: r.hash32(),
    criteriaHash: r.hash32(),
    resultHash: r.hash32(),
    deadline: r.i64(),
    status: r.u8(),
    bump: r.u8(),
  }));
}

async function fetchDecoded<T>(
  connection: Connection,
  address: Address,
  decode: (data: Uint8Array) => T,
): Promise<T | null> {
  const info = await connection.getAccountInfo(toPublicKey(address));
  return info ? decode(info.data) : null;
}

export const fetchAgent = (connection: Connection, address: Address): Promise<AgentAccount | null> =>
  fetchDecoded(connection, address, decodeAgent);
export const fetchConfig = (connection: Connection, address: Address): Promise<ConfigAccount | null> =>
  fetchDecoded(connection, address, decodeConfig);
export const fetchCreditAccount = (connection: Connection, address: Address): Promise<CreditAccount | null> =>
  fetchDecoded(connection, address, decodeCreditAccount);
export const fetchReceiptBatch = (connection: Connection, address: Address): Promise<ReceiptBatchAccount | null> =>
  fetchDecoded(connection, address, decodeReceiptBatch);
export const fetchStakePosition = (connection: Connection, address: Address): Promise<StakePositionAccount | null> =>
  fetchDecoded(connection, address, decodeStakePosition);
export const fetchTask = (connection: Connection, address: Address): Promise<TaskAccount | null> =>
  fetchDecoded(connection, address, decodeTask);
