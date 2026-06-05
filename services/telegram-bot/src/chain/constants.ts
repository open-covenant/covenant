// Covenant `$CVNT` staking — on-chain constants for the stake announcer.
//
// The canonical source for the program id + lock tiers is the SDK
// (`packages/sdk/src/solana/stake.ts`) and the on-chain program
// (`agent-os/programs/stake`). They are re-declared here so the
// telegram-bot deploys as a standalone Render service with no workspace
// dependency — the same trade-off `landing/lib/stake/env.ts` already makes.
// If any of these change on-chain, update them here too.

import { PublicKey } from "@solana/web3.js";

/** Deployed `$CVNT` stake program. */
export const STAKE_PROGRAM_ID = new PublicKey(
  "CstkpU2q9RngbHh21WVAYeQjbN9UWgcH9pAiQcMaEcED",
);

/** Associated Token Program — derives the locked-CVNT vault ATA. */
export const ASSOCIATED_TOKEN_PROGRAM_ID = new PublicKey(
  "ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL",
);

/** Streamflow timelock/vesting program — CVNT team/liquidity vesting locks
 * tokens in per-stream escrows here, which count toward "locked" supply. */
export const STREAMFLOW_PROGRAM_ID = new PublicKey(
  "strmRqUCoQUgGUan5YhzUZa6KqdzwX5L6FpUxfmKg5m",
);
/** Streamflow `Contract` account byte offsets: mint and escrow-token pubkey. */
export const STREAMFLOW_MINT_OFFSET = 177;
export const STREAMFLOW_ESCROW_OFFSET = 209;

/** Token-2022 program — the mainnet `$CVNT` mint is a Token-2022 mint. */
export const TOKEN_2022_PROGRAM_ID = new PublicKey(
  "TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb",
);

/** Legacy SPL Token program — used by the devnet `$CVNT` mint. */
export const SPL_TOKEN_PROGRAM_ID = new PublicKey(
  "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA",
);

/** PDA seed for the locked-CVNT vault authority (`b"vault_auth"`). */
export const VAULT_AUTHORITY_SEED = "vault_auth";

/** PDA seed for the BuyLock vault authority (`b"buylock_auth"`) — buyback CVNT
 * with no withdraw path, i.e. permanently locked. */
export const BUYLOCK_AUTHORITY_SEED = "buylock_auth";

// Lock-tier multiplier bps → human label. Mirrors the on-chain tiers:
// 5000 / 10000 / 15000 / 20000 bps = 7 / 30 / 90 / 180 days.
export const LOCK_TIER_LABELS: Record<number, string> = {
  5000: "7d",
  10000: "30d",
  15000: "90d",
  20000: "180d",
};

export function lockTierLabel(multiplierBps: number): string {
  return LOCK_TIER_LABELS[multiplierBps] ?? `${multiplierBps}bps`;
}

/** `$CVNT` decimals are fixed at 6 by the program (`EXPECTED_MINT_DECIMALS`). */
export const DEFAULT_CVNT_DECIMALS = 6;
