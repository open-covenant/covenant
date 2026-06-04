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
  const authority = lockedVaultAuthority();
  return PublicKey.findProgramAddressSync(
    [authority.toBuffer(), tokenProgramId.toBuffer(), mint.toBuffer()],
    ASSOCIATED_TOKEN_PROGRAM_ID,
  )[0];
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
