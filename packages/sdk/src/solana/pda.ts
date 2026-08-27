import { PublicKey } from '@solana/web3.js';
import { DEFAULT_PROTOCOL_PROGRAM_ID } from '../config.js';
import type { Hash32 } from './accounts.js';
import { toPublicKey, type Address } from './pubkey.js';

export const SETTLEMENT_PROGRAM_ID = DEFAULT_PROTOCOL_PROGRAM_ID;
export const STAKE_PROGRAM_ID = 'CstkpU2q9RngbHh21WVAYeQjbN9UWgcH9pAiQcMaEcED';

export interface DerivedPda {
  address: PublicKey;
  bump: number;
}

const utf8 = (s: string): Buffer => Buffer.from(s, 'utf8');
const hash = (h: Hash32): Buffer => {
  if (!/^[0-9a-f]{64}$/i.test(h)) throw new Error('seed hash must be a 32-byte hex string');
  return Buffer.from(h, 'hex');
};

function derive(seeds: Array<Buffer | Uint8Array>, programId: Address): DerivedPda {
  const [address, bump] = PublicKey.findProgramAddressSync(seeds, toPublicKey(programId));
  return { address, bump };
}

// Settlement-program PDAs (cov9UDyp...).

export function deriveConfigPda(programId: Address = SETTLEMENT_PROGRAM_ID): DerivedPda {
  return derive([utf8('config')], programId);
}

export function deriveAgentPda(agentKey: Hash32, programId: Address = SETTLEMENT_PROGRAM_ID): DerivedPda {
  return derive([utf8('agent'), hash(agentKey)], programId);
}

export function deriveCreditsPda(owner: Address, programId: Address = SETTLEMENT_PROGRAM_ID): DerivedPda {
  return derive([utf8('credits'), toPublicKey(owner).toBuffer()], programId);
}

export function deriveTaskPda(taskId: Hash32, programId: Address = SETTLEMENT_PROGRAM_ID): DerivedPda {
  return derive([utf8('task'), hash(taskId)], programId);
}

export function deriveStakePositionPda(
  agentKey: Hash32,
  owner: Address,
  programId: Address = SETTLEMENT_PROGRAM_ID,
): DerivedPda {
  return derive([utf8('stake'), hash(agentKey), toPublicKey(owner).toBuffer()], programId);
}

export function deriveReceiptBatchPda(batchId: Hash32, programId: Address = SETTLEMENT_PROGRAM_ID): DerivedPda {
  return derive([utf8('receipt_batch'), hash(batchId)], programId);
}

// Compute is staged source and is not deployed at SETTLEMENT_PROGRAM_ID.
// Requiring a program id prevents callers from silently targeting mainnet.
export function deriveComputeConfigPda(programId: Address): DerivedPda {
  return derive([utf8('compute_config')], programId);
}

export function deriveComputeEscrowPda(jobId: Hash32, programId: Address): DerivedPda {
  return derive([utf8('compute_escrow'), hash(jobId)], programId);
}

// Stake-program PDAs (CstkpU2q...).

export function deriveStakeConfigPda(programId: Address = STAKE_PROGRAM_ID): DerivedPda {
  return derive([utf8('stake_config')], programId);
}

export function deriveRewardVaultPda(programId: Address = STAKE_PROGRAM_ID): DerivedPda {
  return derive([utf8('reward_vault')], programId);
}

export function deriveLockedVaultAuthorityPda(programId: Address = STAKE_PROGRAM_ID): DerivedPda {
  return derive([utf8('vault_auth')], programId);
}

export function deriveFeeRouterPda(programId: Address = STAKE_PROGRAM_ID): DerivedPda {
  return derive([utf8('fee_router')], programId);
}

export function deriveBuylockVaultAuthorityPda(programId: Address = STAKE_PROGRAM_ID): DerivedPda {
  return derive([utf8('buylock_auth')], programId);
}

// A stake-program v2 position is keyed by its owner and a caller-chosen u64 nonce.
export function deriveStakeV2PositionPda(
  owner: Address,
  nonce: number | bigint,
  programId: Address = STAKE_PROGRAM_ID,
): DerivedPda {
  if (typeof nonce === 'number' && !Number.isInteger(nonce)) {
    throw new Error(`stake position nonce must be an integer, got ${nonce}`);
  }
  const value = BigInt(nonce);
  if (value < 0n || value > 0xffffffffffffffffn) {
    throw new Error(`stake position nonce must fit in a u64 (0 to 2^64-1), got ${nonce}`);
  }
  const seed = Buffer.alloc(8);
  seed.writeBigUInt64LE(value);
  return derive([utf8('stake_v2'), toPublicKey(owner).toBuffer(), seed], programId);
}

// Token accounts.

export const TOKEN_PROGRAM_ID = 'TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA';
export const ASSOCIATED_TOKEN_PROGRAM_ID = 'ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL';

// The Associated Token Account for owner + mint, so callers derive the token
// accounts the token instructions need instead of passing raw addresses. CVNT is
// a standard SPL token, so the default token program is Tokenkeg; pass Token-2022
// for a Token-2022 mint.
export function deriveAssociatedTokenAddress(
  owner: Address,
  mint: Address,
  tokenProgram: Address = TOKEN_PROGRAM_ID,
): PublicKey {
  const [address] = PublicKey.findProgramAddressSync(
    [toPublicKey(owner).toBuffer(), toPublicKey(tokenProgram).toBuffer(), toPublicKey(mint).toBuffer()],
    toPublicKey(ASSOCIATED_TOKEN_PROGRAM_ID),
  );
  return address;
}
