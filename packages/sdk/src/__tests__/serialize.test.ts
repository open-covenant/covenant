import { describe, expect, it } from 'vitest';
import {
  prepareAnchorReceiptBatchInstruction,
  prepareBuyCreditsInstruction,
  prepareClaimTaskInstruction,
  prepareCreateTaskInstruction,
  prepareRefundTaskInstruction,
  prepareRegisterAgentInstruction,
  prepareReleaseTaskInstruction,
  prepareStakeInstruction,
  prepareSubmitTaskInstruction,
} from '../solana/instructions.js';
import {
  prepareStakeClaimInstruction,
  prepareStakeCreatePositionInstruction,
  prepareStakeInitializeInstruction,
  prepareStakeRotateFeeRouterInstruction,
  STAKE_TIER_30D_BPS,
} from '../solana/stake.js';
import { toTransactionInstruction, toTransactionInstructions } from '../solana/serialize.js';
import type { PreparedSolanaInstruction } from '../solana/instructions.js';

const SYS = '11111111111111111111111111111111'; // 32 zero bytes
const HZERO = '00'.repeat(32);
const HFF = 'ff'.repeat(32);
const hex = (data: Uint8Array) => Buffer.from(data).toString('hex');
const one = <T>(bundle: { instructions: unknown[] }, build = toTransactionInstructions) =>
  build(bundle as never)[0]!;

describe('serialize: golden bytes', () => {
  it('stake maps amount_covnt->amount, keeps lock_until, drops token_symbol', () => {
    const ix = one(
      prepareStakeInstruction({
        configAccount: SYS,
        owner: SYS,
        agentAccount: SYS,
        positionAccount: SYS,
        ownerCovntAccount: SYS,
        stakeVault: SYS,
        covntMint: SYS,
        amountCovnt: '1',
        lockUntil: '2',
      }),
    );
    // discriminator + u64(1) + u64(2); 24 bytes total means token_symbol was not encoded
    expect(hex(ix.data)).toBe('ceb0ca12c8d1b36c' + '0100000000000000' + '0200000000000000');
    expect(ix.data).toHaveLength(24);
  });

  it('buy_credits encodes a single u64', () => {
    const ix = one(
      prepareBuyCreditsInstruction({
        configAccount: SYS,
        owner: SYS,
        creditAccount: SYS,
        ownerCovntAccount: SYS,
        treasury: SYS,
        covntMint: SYS,
        amountCovnt: '255',
      }),
    );
    expect(hex(ix.data)).toBe('0ead3a26f8eb7366' + 'ff00000000000000');
  });

  it('submit_task encodes result and receipt hashes in order', () => {
    const ix = one(
      prepareSubmitTaskInstruction({
        configAccount: SYS,
        taskAccount: SYS,
        provider: SYS,
        resultHash: HZERO,
        receiptHash: HFF,
      }),
    );
    expect(hex(ix.data)).toBe('94b71a746bd576d5' + HZERO + HFF);
  });

  it('release_task carries only its discriminator (no args)', () => {
    const ix = one(
      prepareReleaseTaskInstruction({
        configAccount: SYS,
        taskAccount: SYS,
        authority: SYS,
        escrowVault: SYS,
        providerCovntAccount: SYS,
        covntMint: SYS,
      }),
    );
    expect(hex(ix.data)).toBe('bd768e6325f42677');
  });

  it('claim_task carries only its discriminator (no args)', () => {
    const ix = one(
      prepareClaimTaskInstruction({
        configAccount: SYS,
        taskAccount: SYS,
        provider: SYS,
        escrowVault: SYS,
        providerCovntAccount: SYS,
        covntMint: SYS,
      }),
    );
    expect(hex(ix.data)).toBe('31dedbee9b44dd88');
  });

  it('refund_task carries only its discriminator (no args)', () => {
    const ix = one(
      prepareRefundTaskInstruction({
        configAccount: SYS,
        taskAccount: SYS,
        authority: SYS,
        escrowVault: SYS,
        clientCovntAccount: SYS,
        covntMint: SYS,
      }),
    );
    expect(hex(ix.data)).toBe('080898be18189e15');
  });

  it('create_position flattens the args struct (u64,u64,u16)', () => {
    const ix = one(
      prepareStakeCreatePositionInstruction({
        configAccount: SYS,
        positionAccount: SYS,
        covntMint: SYS,
        lockedVaultAuthority: SYS,
        lockedCvntVault: SYS,
        ownerCvntAta: SYS,
        owner: SYS,
        nonce: '1',
        amount: '2',
        lockTierBps: STAKE_TIER_30D_BPS, // 10000 = 0x2710 -> LE 1027
      }),
    );
    expect(hex(ix.data)).toBe('30d7c59960cbb485' + '0100000000000000' + '0200000000000000' + '1027');
  });

  it('rotate_fee_router encodes three options (none, and a present one)', () => {
    const none = one(
      prepareStakeRotateFeeRouterInstruction({ configAccount: SYS, feeRouterAccount: SYS, authority: SYS }),
    );
    expect(hex(none.data)).toBe('4fbe58eb927845f1' + '00' + '00' + '00');

    const some = one(
      prepareStakeRotateFeeRouterInstruction({
        configAccount: SYS,
        feeRouterAccount: SYS,
        authority: SYS,
        newMaxDepositLamports: '1',
      }),
    );
    expect(hex(some.data)).toBe('4fbe58eb927845f1' + '00' + '01' + '0100000000000000' + '00');
  });
});

describe('serialize: discriminators, lengths, and metas', () => {
  it('register_agent: discriminator, three hashes, and account authority flags', () => {
    const bundle = prepareRegisterAgentInstruction({
      configAccount: SYS,
      operator: SYS,
      agentAccount: SYS,
      agentKey: HZERO,
      metadataHash: HZERO,
      capabilityHash: HZERO,
    });
    const ix = toTransactionInstructions(bundle)[0]!;
    expect(hex(ix.data.subarray(0, 8))).toBe('879d42c30271af1e');
    expect(ix.data).toHaveLength(8 + 96);
    // accounts: [config(-,-), agent(-,w), operator(s,w), system_program(-,-)]
    expect(ix.keys[2]).toMatchObject({ isSigner: true, isWritable: true });
    expect(ix.keys[0]).toMatchObject({ isSigner: false, isWritable: false });
    // the resolved settlement program id is preserved through serialization
    expect(ix.programId.toBase58()).toBe(bundle.instructions[0]!.programId);
  });

  it('create_task: struct with two pubkeys and two i64s flattens to 184 bytes', () => {
    const ix = one(
      prepareCreateTaskInstruction({
        configAccount: SYS,
        client: SYS,
        agentAccount: SYS,
        taskAccount: SYS,
        clientCovntAccount: SYS,
        escrowVault: SYS,
        covntMint: SYS,
        provider: SYS,
        arbiter: SYS,
        taskId: HZERO,
        amountCovnt: '1',
        taskHash: HZERO,
        criteriaHash: HZERO,
        deadline: '1750000000',
        reviewWindow: '3600',
      }),
    );
    expect(hex(ix.data.subarray(0, 8))).toBe('c25006b4e87f30ab');
    expect(ix.data).toHaveLength(8 + 32 + 32 + 32 + 8 + 32 + 32 + 8 + 8);
  });

  it('anchor_receipt_batch: two hashes and a u32', () => {
    const ix = one(
      prepareAnchorReceiptBatchInstruction({
        configAccount: SYS,
        authority: SYS,
        batchAccount: SYS,
        batchId: HZERO,
        merkleRoot: HZERO,
        receiptCount: 12,
      }),
    );
    expect(hex(ix.data.subarray(0, 8))).toBe('5acf5b79393db081');
    expect(hex(ix.data.subarray(-4))).toBe('0c000000');
  });

  it('stake initialize: two pubkeys, two u64, one i64', () => {
    const ix = one(
      prepareStakeInitializeInstruction({
        authority: SYS,
        configAccount: SYS,
        feeRouterAccount: SYS,
        lockedVaultAuthority: SYS,
        rewardVault: SYS,
        buylockVaultAuthority: SYS,
        covntMint: SYS,
        lockedCvntVault: SYS,
        buylockCvntVault: SYS,
        programData: SYS,
        pauseAuthority: SYS,
        feeRouterAuthority: SYS,
        minLockAmount: '1',
        feeRouterMaxDepositLamports: '0',
        feeRouterRateLimitSecs: '0',
      }),
    );
    expect(hex(ix.data.subarray(0, 8))).toBe('afaf6d1f0d989bed');
    expect(ix.data).toHaveLength(8 + 32 + 32 + 8 + 8 + 8);
  });

  it('no-arg instruction (claim) serializes to the bare 8-byte discriminator', () => {
    const ix = one(
      prepareStakeClaimInstruction({ configAccount: SYS, positionAccount: SYS, rewardVault: SYS, owner: SYS }),
    );
    expect(hex(ix.data)).toBe('3ec6d6c1d59f6cd2');
    expect(ix.data).toHaveLength(8);
  });
});

describe('serialize: failure modes', () => {
  it('throws on a descriptor missing a required field (with the descriptor key name)', () => {
    const bundle = prepareStakeInstruction({
      configAccount: SYS,
      owner: SYS,
      agentAccount: SYS,
      positionAccount: SYS,
      ownerCovntAccount: SYS,
      stakeVault: SYS,
      covntMint: SYS,
      amountCovnt: '1',
      lockUntil: '2',
    });
    const prepared = bundle.instructions[0]!;
    delete prepared.data.amount_covnt;
    expect(() => toTransactionInstruction(prepared)).toThrow(/missing field "amount_covnt"/);
  });

  it('throws when the account vector does not match the IDL', () => {
    const broken: PreparedSolanaInstruction = { programId: SYS, instruction: 'stake', accounts: [], data: {} };
    expect(() => toTransactionInstruction(broken)).toThrow(/account mismatch/);
  });

  it('throws on an unknown instruction name', () => {
    const broken: PreparedSolanaInstruction = {
      programId: SYS,
      instruction: 'not_a_real_instruction',
      accounts: [],
      data: {},
    };
    expect(() => toTransactionInstruction(broken)).toThrow(/no covenant IDL instruction/);
  });
});
