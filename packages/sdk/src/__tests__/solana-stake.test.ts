import { describe, expect, it } from 'vitest';
import {
  COVENANT_STAKE_PROGRAM_ID,
  STAKE_TIER_30D_BPS,
  STAKE_TIER_365D_BPS,
  prepareStakeClaimInstruction,
  prepareStakeClosePositionInstruction,
  prepareStakeCreatePositionInstruction,
  prepareStakeDepositSolFeesInstruction,
  prepareStakeIncreaseAmountInstruction,
  prepareStakeRotateFeeRouterInstruction,
} from '../solana/stake.js';

const ADDR = '11111111111111111111111111111111';

describe('Solana stake instruction descriptors', () => {
  it('exposes the covenant-stake program id', () => {
    expect(COVENANT_STAKE_PROGRAM_ID).toBe('CstkpU2q9RngbHh21WVAYeQjbN9UWgcH9pAiQcMaEcED');
  });

  it('builds a create_position bundle with the expected account order', () => {
    const bundle = prepareStakeCreatePositionInstruction({
      configAccount: ADDR,
      positionAccount: ADDR,
      covntMint: ADDR,
      lockedVaultAuthority: ADDR,
      lockedCvntVault: ADDR,
      ownerCvntAta: ADDR,
      owner: ADDR,
      nonce: '1',
      amount: '1000000000',
      lockTierBps: STAKE_TIER_30D_BPS,
    });
    expect(bundle.chain).toBe('solana');
    expect(bundle.instructions[0]!.programId).toBe(COVENANT_STAKE_PROGRAM_ID);
    expect(bundle.instructions[0]!.instruction).toBe('create_position');
    expect(bundle.instructions[0]!.accounts.map((a) => a.name)).toEqual([
      'config',
      'position',
      'covnt_mint',
      'locked_vault_authority',
      'locked_cvnt_vault',
      'owner_cvnt_ata',
      'owner',
      'token_program',
      'system_program',
    ]);
    expect(bundle.instructions[0]!.data.lock_tier_bps).toBe(10_000);
  });

  it('rejects invalid lock tier bps', () => {
    expect(() =>
      prepareStakeCreatePositionInstruction({
        configAccount: ADDR,
        positionAccount: ADDR,
        covntMint: ADDR,
        lockedVaultAuthority: ADDR,
        lockedCvntVault: ADDR,
        ownerCvntAta: ADDR,
        owner: ADDR,
        nonce: '1',
        amount: '1000000000',
        lockTierBps: 12_500 as unknown as typeof STAKE_TIER_30D_BPS,
      }),
    ).toThrow(/invalid stake lock tier/);
  });

  it('builds an increase_amount bundle without a system_program account', () => {
    const bundle = prepareStakeIncreaseAmountInstruction({
      configAccount: ADDR,
      positionAccount: ADDR,
      covntMint: ADDR,
      lockedVaultAuthority: ADDR,
      lockedCvntVault: ADDR,
      ownerCvntAta: ADDR,
      owner: ADDR,
      extra: '500000000',
    });
    expect(bundle.instructions[0]!.instruction).toBe('increase_amount');
    expect(bundle.instructions[0]!.accounts.map((a) => a.name)).not.toContain('system_program');
    expect(bundle.instructions[0]!.data.extra).toBe('500000000');
  });

  it('builds a claim bundle with only the four required accounts', () => {
    const bundle = prepareStakeClaimInstruction({
      configAccount: ADDR,
      positionAccount: ADDR,
      rewardVault: ADDR,
      owner: ADDR,
    });
    expect(bundle.instructions[0]!.accounts).toHaveLength(4);
    expect(bundle.instructions[0]!.accounts.map((a) => a.name)).toEqual([
      'config',
      'position',
      'reward_vault',
      'owner',
    ]);
  });

  it('builds a close_position bundle wiring vault + reward accounts', () => {
    const bundle = prepareStakeClosePositionInstruction({
      configAccount: ADDR,
      positionAccount: ADDR,
      covntMint: ADDR,
      lockedVaultAuthority: ADDR,
      lockedCvntVault: ADDR,
      ownerCvntAta: ADDR,
      rewardVault: ADDR,
      owner: ADDR,
    });
    expect(bundle.instructions[0]!.instruction).toBe('close_position');
    expect(bundle.instructions[0]!.accounts.map((a) => a.name)).toContain('reward_vault');
    expect(bundle.instructions[0]!.accounts.map((a) => a.name)).toContain('locked_cvnt_vault');
  });

  it('builds a deposit_sol_fees bundle gated on the fee router signer', () => {
    const bundle = prepareStakeDepositSolFeesInstruction({
      configAccount: ADDR,
      feeRouterAccount: ADDR,
      rewardVault: ADDR,
      depositor: ADDR,
      amount: '100000000',
    });
    expect(bundle.instructions[0]!.instruction).toBe('deposit_sol_fees');
    const depositor = bundle.instructions[0]!.accounts.find((a) => a.name === 'depositor');
    expect(depositor?.signer).toBe(true);
    expect(depositor?.writable).toBe(true);
  });

  it('encodes optional rotate_fee_router fields as null when absent', () => {
    const bundle = prepareStakeRotateFeeRouterInstruction({
      configAccount: ADDR,
      feeRouterAccount: ADDR,
      authority: ADDR,
    });
    expect(bundle.instructions[0]!.data.new_authority).toBeNull();
    expect(bundle.instructions[0]!.data.new_max_deposit_lamports).toBeNull();
    expect(bundle.instructions[0]!.data.new_rate_limit_secs).toBeNull();
  });

  it('preserves all four tier bps as discriminated literals', () => {
    expect(STAKE_TIER_365D_BPS).toBe(30_000);
  });
});
