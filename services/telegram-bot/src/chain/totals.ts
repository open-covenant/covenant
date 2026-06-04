// Aggregate stake figures for the announcement footer: total `$CVNT` locked
// in the program and what share of supply that is.
//
// Total staked == balance of the locked-CVNT vault, an ATA owned by the
// `vault_auth` PDA. Every `create_position` moves principal in and every
// `close_position` moves it back out, so the vault balance is the live
// active-principal figure. Supply comes from the mint itself.

import { Connection, PublicKey } from "@solana/web3.js";
import {
  ASSOCIATED_TOKEN_PROGRAM_ID,
  BUYLOCK_AUTHORITY_SEED,
  STAKE_PROGRAM_ID,
  VAULT_AUTHORITY_SEED,
} from "./constants.js";

export interface StakeTotals {
  /** Total active staked principal, base units. */
  totalStakedRaw: bigint;
  /** Circulating mint supply, base units. */
  supplyRaw: bigint;
  decimals: number;
  /** Staked share of supply, 0..100 (two-decimal precision). */
  pct: number;
}

function pdaVault(
  authoritySeed: string,
  mint: PublicKey,
  tokenProgramId: PublicKey,
): PublicKey {
  const authority = PublicKey.findProgramAddressSync(
    [Buffer.from(authoritySeed)],
    STAKE_PROGRAM_ID,
  )[0];
  return PublicKey.findProgramAddressSync(
    [authority.toBuffer(), tokenProgramId.toBuffer(), mint.toBuffer()],
    ASSOCIATED_TOKEN_PROGRAM_ID,
  )[0];
}

export function lockedVaultAuthority(): PublicKey {
  return PublicKey.findProgramAddressSync(
    [Buffer.from(VAULT_AUTHORITY_SEED)],
    STAKE_PROGRAM_ID,
  )[0];
}

export function lockedVaultAta(
  mint: PublicKey,
  tokenProgramId: PublicKey,
): PublicKey {
  return pdaVault(VAULT_AUTHORITY_SEED, mint, tokenProgramId);
}

/** BuyLock vault ATA — buyback CVNT locked with no withdraw path. */
export function buylockVaultAta(
  mint: PublicKey,
  tokenProgramId: PublicKey,
): PublicKey {
  return pdaVault(BUYLOCK_AUTHORITY_SEED, mint, tokenProgramId);
}

export async function fetchStakeTotals(
  connection: Connection,
  mint: PublicKey,
  tokenProgramId: PublicKey,
): Promise<StakeTotals> {
  const vault = lockedVaultAta(mint, tokenProgramId);
  const [supply, vaultBalance] = await Promise.all([
    connection.getTokenSupply(mint),
    // The vault is created at program init, but guard anyway so a missing
    // account degrades to "0 staked" instead of throwing the whole poll.
    connection.getTokenAccountBalance(vault).catch(() => null),
  ]);
  const decimals = supply.value.decimals;
  const supplyRaw = BigInt(supply.value.amount);
  const totalStakedRaw = vaultBalance ? BigInt(vaultBalance.value.amount) : 0n;
  // Percent with two-decimal precision via integer math (no float drift on
  // billion-token supplies): staked * 1e6 / supply, then / 1e4.
  const pct =
    supplyRaw > 0n
      ? Number((totalStakedRaw * 1_000_000n) / supplyRaw) / 10_000
      : 0;
  return { totalStakedRaw, supplyRaw, decimals, pct };
}

async function vaultBalance(
  connection: Connection,
  vault: PublicKey,
): Promise<bigint> {
  try {
    return BigInt((await connection.getTokenAccountBalance(vault)).value.amount);
  } catch {
    return 0n; // account may not exist yet
  }
}

export interface StakeSummary {
  /** BuyLock vault — buyback CVNT, permanently locked. Base units. */
  lockedRaw: bigint;
  /** Active staked principal. Base units. */
  stakedRaw: bigint;
  supplyRaw: bigint;
  decimals: number;
  /** (locked + staked) share of supply, 0..100 (two-decimal precision). */
  combinedPct: number;
}

export async function fetchStakeSummary(
  connection: Connection,
  mint: PublicKey,
  tokenProgramId: PublicKey,
): Promise<StakeSummary> {
  const [supply, stakedRaw, lockedRaw] = await Promise.all([
    connection.getTokenSupply(mint),
    vaultBalance(connection, lockedVaultAta(mint, tokenProgramId)),
    vaultBalance(connection, buylockVaultAta(mint, tokenProgramId)),
  ]);
  const supplyRaw = BigInt(supply.value.amount);
  const combinedPct =
    supplyRaw > 0n
      ? Number(((stakedRaw + lockedRaw) * 1_000_000n) / supplyRaw) / 10_000
      : 0;
  return {
    lockedRaw,
    stakedRaw,
    supplyRaw,
    decimals: supply.value.decimals,
    combinedPct,
  };
}
