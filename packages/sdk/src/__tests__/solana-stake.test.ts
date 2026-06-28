import { describe, expect, it } from 'vitest';
import {
  COVENANT_STAKE_PROGRAM_ID,
  STAKE_TIER_7D_BPS,
  STAKE_TIER_30D_BPS,
  STAKE_TIER_90D_BPS,
  STAKE_TIER_180D_BPS,
  prepareStakeAccrueInstruction,
  prepareStakeClaimInstruction,
  prepareStakeClosePositionInstruction,
  prepareStakeCreatePositionInstruction,
  prepareStakeDepositBuylockCvntInstruction,
  prepareStakeDepositSolFeesInstruction,
  prepareStakeIncreaseAmountInstruction,
  prepareStakeInitializeInstruction,
  prepareStakePauseInstruction,
  prepareStakeRotateFeeRouterInstruction,
  prepareStakeUnpauseInstruction,
  prepareStakeUpdateAuthorityInstruction,
  prepareStakeUpdateMaxActiveLocksInstruction,
  prepareStakeUpdateMinLockAmountInstruction,
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
    expect(STAKE_TIER_7D_BPS).toBe(5_000);
    expect(STAKE_TIER_30D_BPS).toBe(10_000);
  });

  it('builds a pause bundle with pause_authority OR authority as signer', () => {
    const bundle = prepareStakePauseInstruction({ configAccount: ADDR, signer: ADDR });
    expect(bundle.instructions[0]!.instruction).toBe('pause');
    expect(bundle.instructions[0]!.accounts.find((a) => a.name === 'signer')?.signer).toBe(true);
  });

  it('builds an unpause bundle restricted to authority', () => {
    const bundle = prepareStakeUnpauseInstruction({ configAccount: ADDR, authority: ADDR });
    expect(bundle.instructions[0]!.instruction).toBe('unpause');
    expect(bundle.instructions[0]!.accounts.find((a) => a.name === 'authority')?.signer).toBe(true);
  });

  it('builds update_min_lock_amount + update_max_active_locks bundles', () => {
    const b1 = prepareStakeUpdateMinLockAmountInstruction({
      configAccount: ADDR,
      authority: ADDR,
      newMin: '5000000000',
    });
    expect(b1.instructions[0]!.instruction).toBe('update_min_lock_amount');
    expect(b1.instructions[0]!.data.new_min).toBe('5000000000');

    const b2 = prepareStakeUpdateMaxActiveLocksInstruction({
      configAccount: ADDR,
      authority: ADDR,
      newMax: 1000,
    });
    expect(b2.instructions[0]!.instruction).toBe('update_max_active_locks');
    expect(b2.instructions[0]!.data.new_max).toBe(1000);
  });
});

// These four builders are absent from solana-stake.test.ts above and from the
// sdk-compatibility instruction fixture (which only parses instructions.ts), so
// nothing else pins their account order, signer/writable authorization flags, or
// data bindings. Distinct per-account addresses also catch input-misrouting that
// the shared-ADDR cases above cannot.
const SYSTEM_PROGRAM = '11111111111111111111111111111111';
const TOKEN_PROGRAM = 'TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA';
const A = (c: string) => c.repeat(32);

describe('Solana stake descriptors — previously-uncovered builders', () => {
  it('initialize: authority is the sole signer and seeds authority data fields', () => {
    const ix = prepareStakeInitializeInstruction({
      configAccount: A('2'),
      feeRouterAccount: A('3'),
      lockedVaultAuthority: A('4'),
      rewardVault: A('5'),
      buylockVaultAuthority: A('6'),
      covntMint: A('7'),
      lockedCvntVault: A('8'),
      buylockCvntVault: A('9'),
      authority: A('A'),
      programData: A('B'),
      pauseAuthority: A('C'),
      feeRouterAuthority: A('D'),
      minLockAmount: '1000',
      feeRouterMaxDepositLamports: '2000',
      feeRouterRateLimitSecs: '300',
    }).instructions[0]!;

    expect(ix.programId).toBe(COVENANT_STAKE_PROGRAM_ID);
    expect(ix.instruction).toBe('initialize');
    expect(ix.accounts).toEqual([
      { name: 'config', address: A('2'), signer: false, writable: true },
      { name: 'fee_router', address: A('3'), signer: false, writable: true },
      { name: 'locked_vault_authority', address: A('4'), signer: false, writable: false },
      { name: 'reward_vault', address: A('5'), signer: false, writable: true },
      { name: 'buylock_vault_authority', address: A('6'), signer: false, writable: false },
      { name: 'covnt_mint', address: A('7'), signer: false, writable: false },
      { name: 'locked_cvnt_vault', address: A('8'), signer: false, writable: false },
      { name: 'buylock_cvnt_vault', address: A('9'), signer: false, writable: false },
      { name: 'authority', address: A('A'), signer: true, writable: true },
      { name: 'program_data', address: A('B'), signer: false, writable: false },
      { name: 'token_program', address: TOKEN_PROGRAM, signer: false, writable: false },
      { name: 'system_program', address: SYSTEM_PROGRAM, signer: false, writable: false },
    ]);
    expect(ix.data).toEqual({
      pause_authority: A('C'),
      fee_router_authority: A('D'),
      min_lock_amount: '1000',
      fee_router_max_deposit_lamports: '2000',
      fee_router_rate_limit_secs: '300',
    });
  });

  it('update_authority: authority signs read-only and binds the new authority', () => {
    const ix = prepareStakeUpdateAuthorityInstruction({
      configAccount: A('2'),
      authority: A('3'),
      newAuthority: A('4'),
    }).instructions[0]!;

    expect(ix.programId).toBe(COVENANT_STAKE_PROGRAM_ID);
    expect(ix.instruction).toBe('update_authority');
    expect(ix.accounts).toEqual([
      { name: 'config', address: A('2'), signer: false, writable: true },
      { name: 'authority', address: A('3'), signer: true, writable: false },
    ]);
    expect(ix.data).toEqual({ new_authority: A('4') });
  });

  it('accrue: a single writable config account and empty data', () => {
    const ix = prepareStakeAccrueInstruction({ configAccount: A('2') }).instructions[0]!;

    expect(ix.programId).toBe(COVENANT_STAKE_PROGRAM_ID);
    expect(ix.instruction).toBe('accrue');
    expect(ix.accounts).toEqual([
      { name: 'config', address: A('2'), signer: false, writable: true },
    ]);
    expect(ix.data).toEqual({});
  });

  it('deposit_buylock_cvnt: depositor is the sole signer moving CVNT into the vault', () => {
    const ix = prepareStakeDepositBuylockCvntInstruction({
      configAccount: A('2'),
      feeRouterAccount: A('3'),
      covntMint: A('4'),
      buylockVaultAuthority: A('5'),
      buylockCvntVault: A('6'),
      depositorCvntAta: A('7'),
      depositor: A('8'),
      amount: '100',
    }).instructions[0]!;

    expect(ix.programId).toBe(COVENANT_STAKE_PROGRAM_ID);
    expect(ix.instruction).toBe('deposit_buylock_cvnt');
    expect(ix.accounts).toEqual([
      { name: 'config', address: A('2'), signer: false, writable: true },
      { name: 'fee_router', address: A('3'), signer: false, writable: false },
      { name: 'covnt_mint', address: A('4'), signer: false, writable: false },
      { name: 'buylock_vault_authority', address: A('5'), signer: false, writable: false },
      { name: 'buylock_cvnt_vault', address: A('6'), signer: false, writable: true },
      { name: 'depositor_cvnt_ata', address: A('7'), signer: false, writable: true },
      { name: 'depositor', address: A('8'), signer: true, writable: true },
      { name: 'token_program', address: TOKEN_PROGRAM, signer: false, writable: false },
    ]);
    expect(ix.data).toEqual({ amount: '100' });
  });
});

// The rotate_fee_router builder only had its all-absent path pinned (the data
// nulls above); the present-value path, its account order, and the three
// non-30d lock-tier accept arms of assertLockTier were unexercised.
describe('Solana stake descriptors — remaining branch coverage', () => {
  it('rotate_fee_router: present optional fields pass through verbatim and pin account order', () => {
    const ix = prepareStakeRotateFeeRouterInstruction({
      configAccount: A('2'),
      feeRouterAccount: A('3'),
      authority: A('4'),
      newAuthority: A('5'),
      newMaxDepositLamports: '9000',
      newRateLimitSecs: '600',
    }).instructions[0]!;

    expect(ix.programId).toBe(COVENANT_STAKE_PROGRAM_ID);
    expect(ix.instruction).toBe('rotate_fee_router');
    expect(ix.accounts).toEqual([
      { name: 'config', address: A('2'), signer: false, writable: false },
      { name: 'fee_router', address: A('3'), signer: false, writable: true },
      { name: 'authority', address: A('4'), signer: true, writable: false },
    ]);
    expect(ix.data).toEqual({
      new_authority: A('5'),
      new_max_deposit_lamports: '9000',
      new_rate_limit_secs: '600',
    });
  });

  it('rotate_fee_router: each optional field coalesces independently (mixed present/absent)', () => {
    const ix = prepareStakeRotateFeeRouterInstruction({
      configAccount: A('2'),
      feeRouterAccount: A('3'),
      authority: A('4'),
      newAuthority: A('5'),
      newRateLimitSecs: '600',
    }).instructions[0]!;

    // Distinct present values in their own slots prove no field reads another's
    // input, and the absent middle field still resolves to null.
    expect(ix.data).toEqual({
      new_authority: A('5'),
      new_max_deposit_lamports: null,
      new_rate_limit_secs: '600',
    });
  });

  it('create_position: assertLockTier admits every non-30d tier (7d/90d/180d accept arms)', () => {
    const tiers = [
      [STAKE_TIER_7D_BPS, 5_000],
      [STAKE_TIER_90D_BPS, 15_000],
      [STAKE_TIER_180D_BPS, 20_000],
    ] as const;
    for (const [tier, bps] of tiers) {
      const ix = prepareStakeCreatePositionInstruction({
        configAccount: A('2'),
        positionAccount: A('3'),
        covntMint: A('4'),
        lockedVaultAuthority: A('5'),
        lockedCvntVault: A('6'),
        ownerCvntAta: A('7'),
        owner: A('8'),
        nonce: '1',
        amount: '1000000000',
        lockTierBps: tier,
      }).instructions[0]!;
      expect(ix.data.lock_tier_bps).toBe(bps);
    }
  });
});
