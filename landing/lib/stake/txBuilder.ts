import {
  PublicKey,
  SystemProgram,
  TransactionInstruction,
} from "@solana/web3.js";
import {
  anchorDiscriminator,
  concatBytes,
  u16le,
  u64le,
} from "./anchor";
import { getClusterConfig } from "./env";
import {
  configPda,
  deriveAta,
  feeRouterPda,
  lockedVaultAuthorityPda,
  positionPda,
  rewardVaultPda,
} from "./pdas";

export const TIER_30D_BPS = 10_000;
export const TIER_90D_BPS = 15_000;
export const TIER_180D_BPS = 20_000;
export const TIER_365D_BPS = 30_000;

export const TIER_OPTIONS: { label: string; days: number; bps: number }[] = [
  { label: "30 days · 1.0x", days: 30, bps: TIER_30D_BPS },
  { label: "90 days · 1.5x", days: 90, bps: TIER_90D_BPS },
  { label: "180 days · 2.0x", days: 180, bps: TIER_180D_BPS },
  { label: "365 days · 3.0x", days: 365, bps: TIER_365D_BPS },
];

export function buildCreatePositionIx(opts: {
  owner: PublicKey;
  nonce: bigint;
  amount: bigint;
  lockTierBps: number;
}): TransactionInstruction {
  const { owner, nonce, amount, lockTierBps } = opts;
  const { cvntMint, tokenProgramId } = getClusterConfig();
  const lockedVault = deriveAta(
    lockedVaultAuthorityPda(),
    cvntMint,
    tokenProgramId,
  );
  const ownerAta = deriveAta(owner, cvntMint, tokenProgramId);
  const position = positionPda(owner, nonce);

  const data = concatBytes(
    anchorDiscriminator("create_position"),
    u64le(nonce),
    u64le(amount),
    u16le(lockTierBps),
  );

  return new TransactionInstruction({
    programId: getClusterConfig().cluster
      ? STAKE_PROGRAM_ID_FOR_TX()
      : STAKE_PROGRAM_ID_FOR_TX(),
    keys: [
      { pubkey: configPda(), isSigner: false, isWritable: true },
      { pubkey: position, isSigner: false, isWritable: true },
      { pubkey: cvntMint, isSigner: false, isWritable: false },
      { pubkey: lockedVaultAuthorityPda(), isSigner: false, isWritable: false },
      { pubkey: lockedVault, isSigner: false, isWritable: true },
      { pubkey: ownerAta, isSigner: false, isWritable: true },
      { pubkey: owner, isSigner: true, isWritable: true },
      { pubkey: tokenProgramId, isSigner: false, isWritable: false },
      { pubkey: SystemProgram.programId, isSigner: false, isWritable: false },
    ],
    data: Buffer.from(data),
  });
}

export function buildClaimIx(opts: {
  owner: PublicKey;
  nonce: bigint;
}): TransactionInstruction {
  const { owner, nonce } = opts;
  const data = anchorDiscriminator("claim");
  return new TransactionInstruction({
    programId: STAKE_PROGRAM_ID_FOR_TX(),
    keys: [
      { pubkey: configPda(), isSigner: false, isWritable: true },
      {
        pubkey: positionPda(owner, nonce),
        isSigner: false,
        isWritable: true,
      },
      { pubkey: rewardVaultPda(), isSigner: false, isWritable: true },
      { pubkey: owner, isSigner: true, isWritable: true },
    ],
    data: Buffer.from(data),
  });
}

export function buildClosePositionIx(opts: {
  owner: PublicKey;
  nonce: bigint;
}): TransactionInstruction {
  const { owner, nonce } = opts;
  const { cvntMint, tokenProgramId } = getClusterConfig();
  const lockedVault = deriveAta(
    lockedVaultAuthorityPda(),
    cvntMint,
    tokenProgramId,
  );
  const ownerAta = deriveAta(owner, cvntMint, tokenProgramId);
  const data = anchorDiscriminator("close_position");
  return new TransactionInstruction({
    programId: STAKE_PROGRAM_ID_FOR_TX(),
    keys: [
      { pubkey: configPda(), isSigner: false, isWritable: true },
      {
        pubkey: positionPda(owner, nonce),
        isSigner: false,
        isWritable: true,
      },
      { pubkey: cvntMint, isSigner: false, isWritable: false },
      { pubkey: lockedVaultAuthorityPda(), isSigner: false, isWritable: false },
      { pubkey: lockedVault, isSigner: false, isWritable: true },
      { pubkey: ownerAta, isSigner: false, isWritable: true },
      { pubkey: rewardVaultPda(), isSigner: false, isWritable: true },
      { pubkey: owner, isSigner: true, isWritable: true },
      { pubkey: tokenProgramId, isSigner: false, isWritable: false },
    ],
    data: Buffer.from(data),
  });
}

export function buildCreateAtaIx(opts: {
  payer: PublicKey;
  owner: PublicKey;
}): TransactionInstruction {
  const { cvntMint, tokenProgramId } = getClusterConfig();
  const ata = deriveAta(opts.owner, cvntMint, tokenProgramId);
  return new TransactionInstruction({
    programId: new PublicKey("ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL"),
    keys: [
      { pubkey: opts.payer, isSigner: true, isWritable: true },
      { pubkey: ata, isSigner: false, isWritable: true },
      { pubkey: opts.owner, isSigner: false, isWritable: false },
      { pubkey: cvntMint, isSigner: false, isWritable: false },
      { pubkey: SystemProgram.programId, isSigner: false, isWritable: false },
      { pubkey: tokenProgramId, isSigner: false, isWritable: false },
    ],
    data: Buffer.from([]),
  });
}

function STAKE_PROGRAM_ID_FOR_TX(): PublicKey {
  return new PublicKey("CstkpU2q9RngbHh21WVAYeQjbN9UWgcH9pAiQcMaEcED");
}
