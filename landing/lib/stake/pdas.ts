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
  const nonceBytes = new Uint8Array(8);
  new DataView(nonceBytes.buffer).setBigUint64(0, nonce, true);
  return PublicKey.findProgramAddressSync(
    [Buffer.from("stake_v2"), owner.toBuffer(), Buffer.from(nonceBytes)],
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
