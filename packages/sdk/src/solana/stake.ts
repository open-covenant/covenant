import {
  assertSolanaAddress,
  type SolanaAddress,
} from './accounts.js';
import {
  type PreparedAccountMeta,
  type PreparedSolanaBundle,
  type PreparedSolanaInstruction,
} from './instructions.js';
import { resolveSolanaNetwork } from './network.js';

export const COVENANT_STAKE_PROGRAM_ID: SolanaAddress =
  'CstkpU2q9RngbHh21WVAYeQjbN9UWgcH9pAiQcMaEcED' as SolanaAddress;

const SYSTEM_PROGRAM_ID = '11111111111111111111111111111111';
const TOKEN_PROGRAM_ID = 'TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA';

export const STAKE_TIER_30D_BPS = 10_000;
export const STAKE_TIER_90D_BPS = 15_000;
export const STAKE_TIER_180D_BPS = 20_000;
export const STAKE_TIER_365D_BPS = 30_000;

export type StakeLockTierBps =
  | typeof STAKE_TIER_30D_BPS
  | typeof STAKE_TIER_90D_BPS
  | typeof STAKE_TIER_180D_BPS
  | typeof STAKE_TIER_365D_BPS;

export interface StakeInitializeInput {
  authority: SolanaAddress;
  configAccount: SolanaAddress;
  feeRouterAccount: SolanaAddress;
  lockedVaultAuthority: SolanaAddress;
  rewardVault: SolanaAddress;
  buylockVaultAuthority: SolanaAddress;
  covntMint: SolanaAddress;
  lockedCvntVault: SolanaAddress;
  buylockCvntVault: SolanaAddress;
  programData: SolanaAddress;
  pauseAuthority: SolanaAddress;
  feeRouterAuthority: SolanaAddress;
  minLockAmount: string;
  feeRouterMaxDepositLamports: string;
  feeRouterRateLimitSecs: string;
}

export interface StakePauseInput {
  configAccount: SolanaAddress;
  signer: SolanaAddress;
}

export interface StakeUnpauseInput {
  configAccount: SolanaAddress;
  authority: SolanaAddress;
}

export interface StakeUpdateMinLockAmountInput {
  configAccount: SolanaAddress;
  authority: SolanaAddress;
  newMin: string;
}

export interface StakeUpdateMaxActiveLocksInput {
  configAccount: SolanaAddress;
  authority: SolanaAddress;
  newMax: number;
}

export interface StakeUpdateAuthorityInput {
  configAccount: SolanaAddress;
  authority: SolanaAddress;
  newAuthority: SolanaAddress;
}

export interface StakeRotateFeeRouterInput {
  configAccount: SolanaAddress;
  feeRouterAccount: SolanaAddress;
  authority: SolanaAddress;
  newAuthority?: SolanaAddress;
  newMaxDepositLamports?: string;
  newRateLimitSecs?: string;
}

export interface StakeCreatePositionInput {
  configAccount: SolanaAddress;
  positionAccount: SolanaAddress;
  covntMint: SolanaAddress;
  lockedVaultAuthority: SolanaAddress;
  lockedCvntVault: SolanaAddress;
  ownerCvntAta: SolanaAddress;
  owner: SolanaAddress;
  nonce: string;
  amount: string;
  lockTierBps: StakeLockTierBps;
}

export interface StakeIncreaseAmountInput {
  configAccount: SolanaAddress;
  positionAccount: SolanaAddress;
  covntMint: SolanaAddress;
  lockedVaultAuthority: SolanaAddress;
  lockedCvntVault: SolanaAddress;
  ownerCvntAta: SolanaAddress;
  owner: SolanaAddress;
  extra: string;
}

export interface StakeClaimInput {
  configAccount: SolanaAddress;
  positionAccount: SolanaAddress;
  rewardVault: SolanaAddress;
  owner: SolanaAddress;
}

export interface StakeClosePositionInput {
  configAccount: SolanaAddress;
  positionAccount: SolanaAddress;
  covntMint: SolanaAddress;
  lockedVaultAuthority: SolanaAddress;
  lockedCvntVault: SolanaAddress;
  ownerCvntAta: SolanaAddress;
  rewardVault: SolanaAddress;
  owner: SolanaAddress;
}

export interface StakeDepositSolFeesInput {
  configAccount: SolanaAddress;
  feeRouterAccount: SolanaAddress;
  rewardVault: SolanaAddress;
  depositor: SolanaAddress;
  amount: string;
}

export interface StakeAccrueInput {
  configAccount: SolanaAddress;
}

export interface StakeDepositBuylockCvntInput {
  configAccount: SolanaAddress;
  feeRouterAccount: SolanaAddress;
  covntMint: SolanaAddress;
  buylockVaultAuthority: SolanaAddress;
  buylockCvntVault: SolanaAddress;
  depositorCvntAta: SolanaAddress;
  depositor: SolanaAddress;
  amount: string;
}

export function prepareStakeInitializeInstruction(
  input: StakeInitializeInput,
): PreparedSolanaBundle {
  assertLockTier(STAKE_TIER_30D_BPS);
  return bundle({
    programId: COVENANT_STAKE_PROGRAM_ID,
    instruction: 'initialize',
    accounts: [
      meta('config', input.configAccount, false, true),
      meta('fee_router', input.feeRouterAccount, false, true),
      meta('locked_vault_authority', input.lockedVaultAuthority, false, false),
      meta('reward_vault', input.rewardVault, false, true),
      meta('buylock_vault_authority', input.buylockVaultAuthority, false, false),
      meta('covnt_mint', input.covntMint, false, false),
      meta('locked_cvnt_vault', input.lockedCvntVault, false, false),
      meta('buylock_cvnt_vault', input.buylockCvntVault, false, false),
      meta('authority', input.authority, true, true),
      meta('program_data', input.programData, false, false),
      meta('token_program', TOKEN_PROGRAM_ID, false, false),
      meta('system_program', SYSTEM_PROGRAM_ID, false, false),
    ],
    data: {
      pause_authority: input.pauseAuthority,
      fee_router_authority: input.feeRouterAuthority,
      min_lock_amount: input.minLockAmount,
      fee_router_max_deposit_lamports: input.feeRouterMaxDepositLamports,
      fee_router_rate_limit_secs: input.feeRouterRateLimitSecs,
    },
  });
}

export function prepareStakePauseInstruction(input: StakePauseInput): PreparedSolanaBundle {
  return bundle({
    programId: COVENANT_STAKE_PROGRAM_ID,
    instruction: 'pause',
    accounts: [
      meta('config', input.configAccount, false, true),
      meta('signer', input.signer, true, false),
    ],
    data: {},
  });
}

export function prepareStakeUnpauseInstruction(input: StakeUnpauseInput): PreparedSolanaBundle {
  return bundle({
    programId: COVENANT_STAKE_PROGRAM_ID,
    instruction: 'unpause',
    accounts: [
      meta('config', input.configAccount, false, true),
      meta('authority', input.authority, true, false),
    ],
    data: {},
  });
}

export function prepareStakeUpdateMinLockAmountInstruction(
  input: StakeUpdateMinLockAmountInput,
): PreparedSolanaBundle {
  return bundle({
    programId: COVENANT_STAKE_PROGRAM_ID,
    instruction: 'update_min_lock_amount',
    accounts: [
      meta('config', input.configAccount, false, true),
      meta('authority', input.authority, true, false),
    ],
    data: { new_min: input.newMin },
  });
}

export function prepareStakeUpdateMaxActiveLocksInstruction(
  input: StakeUpdateMaxActiveLocksInput,
): PreparedSolanaBundle {
  return bundle({
    programId: COVENANT_STAKE_PROGRAM_ID,
    instruction: 'update_max_active_locks',
    accounts: [
      meta('config', input.configAccount, false, true),
      meta('authority', input.authority, true, false),
    ],
    data: { new_max: input.newMax },
  });
}

export function prepareStakeUpdateAuthorityInstruction(
  input: StakeUpdateAuthorityInput,
): PreparedSolanaBundle {
  return bundle({
    programId: COVENANT_STAKE_PROGRAM_ID,
    instruction: 'update_authority',
    accounts: [
      meta('config', input.configAccount, false, true),
      meta('authority', input.authority, true, false),
    ],
    data: { new_authority: input.newAuthority },
  });
}

export function prepareStakeRotateFeeRouterInstruction(
  input: StakeRotateFeeRouterInput,
): PreparedSolanaBundle {
  return bundle({
    programId: COVENANT_STAKE_PROGRAM_ID,
    instruction: 'rotate_fee_router',
    accounts: [
      meta('config', input.configAccount, false, false),
      meta('fee_router', input.feeRouterAccount, false, true),
      meta('authority', input.authority, true, false),
    ],
    data: {
      new_authority: input.newAuthority ?? null,
      new_max_deposit_lamports: input.newMaxDepositLamports ?? null,
      new_rate_limit_secs: input.newRateLimitSecs ?? null,
    },
  });
}

export function prepareStakeCreatePositionInstruction(
  input: StakeCreatePositionInput,
): PreparedSolanaBundle {
  assertLockTier(input.lockTierBps);
  return bundle({
    programId: COVENANT_STAKE_PROGRAM_ID,
    instruction: 'create_position',
    accounts: [
      meta('config', input.configAccount, false, true),
      meta('position', input.positionAccount, false, true),
      meta('covnt_mint', input.covntMint, false, false),
      meta('locked_vault_authority', input.lockedVaultAuthority, false, false),
      meta('locked_cvnt_vault', input.lockedCvntVault, false, true),
      meta('owner_cvnt_ata', input.ownerCvntAta, false, true),
      meta('owner', input.owner, true, true),
      meta('token_program', TOKEN_PROGRAM_ID, false, false),
      meta('system_program', SYSTEM_PROGRAM_ID, false, false),
    ],
    data: {
      nonce: input.nonce,
      amount: input.amount,
      lock_tier_bps: input.lockTierBps,
    },
  });
}

export function prepareStakeIncreaseAmountInstruction(
  input: StakeIncreaseAmountInput,
): PreparedSolanaBundle {
  return bundle({
    programId: COVENANT_STAKE_PROGRAM_ID,
    instruction: 'increase_amount',
    accounts: [
      meta('config', input.configAccount, false, true),
      meta('position', input.positionAccount, false, true),
      meta('covnt_mint', input.covntMint, false, false),
      meta('locked_vault_authority', input.lockedVaultAuthority, false, false),
      meta('locked_cvnt_vault', input.lockedCvntVault, false, true),
      meta('owner_cvnt_ata', input.ownerCvntAta, false, true),
      meta('owner', input.owner, true, false),
      meta('token_program', TOKEN_PROGRAM_ID, false, false),
    ],
    data: { extra: input.extra },
  });
}

export function prepareStakeClaimInstruction(input: StakeClaimInput): PreparedSolanaBundle {
  return bundle({
    programId: COVENANT_STAKE_PROGRAM_ID,
    instruction: 'claim',
    accounts: [
      meta('config', input.configAccount, false, true),
      meta('position', input.positionAccount, false, true),
      meta('reward_vault', input.rewardVault, false, true),
      meta('owner', input.owner, true, true),
    ],
    data: {},
  });
}

export function prepareStakeClosePositionInstruction(
  input: StakeClosePositionInput,
): PreparedSolanaBundle {
  return bundle({
    programId: COVENANT_STAKE_PROGRAM_ID,
    instruction: 'close_position',
    accounts: [
      meta('config', input.configAccount, false, true),
      meta('position', input.positionAccount, false, true),
      meta('covnt_mint', input.covntMint, false, false),
      meta('locked_vault_authority', input.lockedVaultAuthority, false, false),
      meta('locked_cvnt_vault', input.lockedCvntVault, false, true),
      meta('owner_cvnt_ata', input.ownerCvntAta, false, true),
      meta('reward_vault', input.rewardVault, false, true),
      meta('owner', input.owner, true, true),
      meta('token_program', TOKEN_PROGRAM_ID, false, false),
    ],
    data: {},
  });
}

export function prepareStakeDepositSolFeesInstruction(
  input: StakeDepositSolFeesInput,
): PreparedSolanaBundle {
  return bundle({
    programId: COVENANT_STAKE_PROGRAM_ID,
    instruction: 'deposit_sol_fees',
    accounts: [
      meta('config', input.configAccount, false, true),
      meta('fee_router', input.feeRouterAccount, false, true),
      meta('reward_vault', input.rewardVault, false, true),
      meta('depositor', input.depositor, true, true),
      meta('system_program', SYSTEM_PROGRAM_ID, false, false),
    ],
    data: { amount: input.amount },
  });
}

export function prepareStakeAccrueInstruction(input: StakeAccrueInput): PreparedSolanaBundle {
  return bundle({
    programId: COVENANT_STAKE_PROGRAM_ID,
    instruction: 'accrue',
    accounts: [meta('config', input.configAccount, false, true)],
    data: {},
  });
}

export function prepareStakeDepositBuylockCvntInstruction(
  input: StakeDepositBuylockCvntInput,
): PreparedSolanaBundle {
  return bundle({
    programId: COVENANT_STAKE_PROGRAM_ID,
    instruction: 'deposit_buylock_cvnt',
    accounts: [
      meta('config', input.configAccount, false, true),
      meta('fee_router', input.feeRouterAccount, false, false),
      meta('covnt_mint', input.covntMint, false, false),
      meta('buylock_vault_authority', input.buylockVaultAuthority, false, false),
      meta('buylock_cvnt_vault', input.buylockCvntVault, false, true),
      meta('depositor_cvnt_ata', input.depositorCvntAta, false, true),
      meta('depositor', input.depositor, true, true),
      meta('token_program', TOKEN_PROGRAM_ID, false, false),
    ],
    data: { amount: input.amount },
  });
}

function assertLockTier(tier: number): asserts tier is StakeLockTierBps {
  if (
    tier !== STAKE_TIER_30D_BPS &&
    tier !== STAKE_TIER_90D_BPS &&
    tier !== STAKE_TIER_180D_BPS &&
    tier !== STAKE_TIER_365D_BPS
  ) {
    throw new Error(`invalid stake lock tier bps: ${tier}`);
  }
}

function bundle(instruction: PreparedSolanaInstruction): PreparedSolanaBundle {
  const network = resolveSolanaNetwork();
  return {
    chain: 'solana',
    cluster: network.cluster,
    rpcUrl: network.rpcUrl,
    instructions: [instruction],
  };
}

function meta(
  name: string,
  address: string,
  signer: boolean,
  writable: boolean,
): PreparedAccountMeta {
  return {
    name,
    address: assertSolanaAddress(address, name),
    signer,
    writable,
  };
}
