import { PublicKey } from "@solana/web3.js";
import {
  ASSOCIATED_TOKEN_PROGRAM_ID,
  STAKE_PROGRAM_ID,
  getClusterConfig,
} from "./env";

export function configPda(): PublicKey {
  return PublicKey.findProgramAddressSync(
    [Buffer.from("stake_config")],
    STAKE_PROGRAM_ID,
  )[0];
}

export function feeRouterPda(): PublicKey {
  return PublicKey.findProgramAddressSync(
    [Buffer.from("fee_router")],
    STAKE_PROGRAM_ID,
  )[0];
}

export function rewardVaultPda(): PublicKey {
  return PublicKey.findProgramAddressSync(
    [Buffer.from("reward_vault")],
    STAKE_PROGRAM_ID,
  )[0];
}

export function lockedVaultAuthorityPda(): PublicKey {
  return PublicKey.findProgramAddressSync(
    [Buffer.from("vault_auth")],
    STAKE_PROGRAM_ID,
  )[0];
}

export function buylockVaultAuthorityPda(): PublicKey {
  return PublicKey.findProgramAddressSync(
    [Buffer.from("buylock_auth")],
    STAKE_PROGRAM_ID,
  )[0];
}

export function positionPda(owner: PublicKey, nonce: bigint): PublicKey {
  const nonceBuf = Buffer.alloc(8);
  nonceBuf.writeBigUInt64LE(nonce);
  return PublicKey.findProgramAddressSync(
    [Buffer.from("stake_v2"), owner.toBuffer(), nonceBuf],
    STAKE_PROGRAM_ID,
  )[0];
}

export function deriveAta(
  owner: PublicKey,
  mint: PublicKey,
  tokenProgramId?: PublicKey,
): PublicKey {
  const program = tokenProgramId ?? getClusterConfig().tokenProgramId;
  return PublicKey.findProgramAddressSync(
    [owner.toBuffer(), program.toBuffer(), mint.toBuffer()],
    ASSOCIATED_TOKEN_PROGRAM_ID,
  )[0];
}
