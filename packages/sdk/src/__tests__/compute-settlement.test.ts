import { describe, expect, it } from 'vitest';
import { concat, hash32, i64, pubkey, u64 } from '../solana/borsh.js';
import { decodeComputeEscrow, decodeComputePaymentConfig } from '../solana/decode.js';
import {
  prepareFundComputeJobInstruction,
  prepareInitializeComputePaymentsInstruction,
  prepareRefundComputeJobInstruction,
  prepareSettleComputeJobInstruction,
  prepareUpdateComputeSettlementAuthorityInstruction,
  type ComputeProgramDeployment,
  type FundComputeJobInput,
} from '../solana/instructions.js';
import {
  deriveComputeConfigPda,
  deriveComputeEscrowPda,
  SETTLEMENT_PROGRAM_ID,
} from '../solana/pda.js';
import { toTransactionInstructions } from '../solana/serialize.js';

const PROGRAM_ID = 'BPFLoaderUpgradeab1e11111111111111111111111';
const SYSTEM_PROGRAM_ID = '11111111111111111111111111111111';
const TOKEN_PROGRAM_ID = 'TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA';
const TOKEN_2022_PROGRAM_ID = 'TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb';
const CLIENT = 'So11111111111111111111111111111111111111112';
const JOB_ID = '11'.repeat(32);
const QUOTE_COMMITMENT = '22'.repeat(32);
const RECEIPT_COMMITMENT = '33'.repeat(32);
const REFUND_COMMITMENT = '44'.repeat(32);
const deployment: ComputeProgramDeployment = {
  programId: PROGRAM_ID,
  cluster: 'localnet',
  rpcUrl: 'http://127.0.0.1:8899',
};

const hex = (data: Uint8Array): string => Buffer.from(data).toString('hex');
const only = (bundle: ReturnType<typeof prepareFundComputeJobInstruction>) =>
  bundle.instructions[0]!;

const fundInput = (): FundComputeJobInput => ({
  deployment,
  configAccount: SYSTEM_PROGRAM_ID,
  computeConfigAccount: SYSTEM_PROGRAM_ID,
  escrowAccount: SYSTEM_PROGRAM_ID,
  client: SYSTEM_PROGRAM_ID,
  clientUsdcAccount: SYSTEM_PROGRAM_ID,
  providerUsdcAccount: SYSTEM_PROGRAM_ID,
  escrowVault: SYSTEM_PROGRAM_ID,
  usdcMint: SYSTEM_PROGRAM_ID,
  jobId: JOB_ID,
  quoteCommitment: QUOTE_COMMITMENT,
  provider: SYSTEM_PROGRAM_ID,
  maxUsdcAmount: '1',
  expiresAt: '2',
});

describe('compute settlement addresses', () => {
  it('derives the generated IDL seeds against an explicit program id', () => {
    const config = deriveComputeConfigPda(PROGRAM_ID);
    const escrow = deriveComputeEscrowPda('22'.repeat(32), SYSTEM_PROGRAM_ID, PROGRAM_ID);

    expect(config.address.toBase58()).toBe('ER3t6Cxz5AQ3cS8nJGPyj5PpGpui1kRMchr7wn1BsGuM');
    expect(config.bump).toBe(255);
    expect(escrow.address.toBase58()).toBe('GzUf6BWcfg15DbE55VVfsTtE2vkomueEoYAMzRn4jxso');
    expect(escrow.bump).toBe(255);
  });

  it('scopes the escrow to the client so a job id cannot be squatted', () => {
    const mine = deriveComputeEscrowPda('22'.repeat(32), SYSTEM_PROGRAM_ID, PROGRAM_ID);
    const theirs = deriveComputeEscrowPda('22'.repeat(32), CLIENT, PROGRAM_ID);
    expect(mine.address.equals(theirs.address)).toBe(false);
  });
});

describe('compute settlement instruction descriptors', () => {
  it('requires an explicit deployment instead of defaulting to current mainnet', () => {
    const input = { ...fundInput(), deployment: undefined } as unknown as FundComputeJobInput;
    expect(() => prepareFundComputeJobInstruction(input)).toThrow(/explicit compute deployment/);
  });

  it('rejects the current mainnet deployment until compute is actually deployed', () => {
    const input = {
      ...fundInput(),
      deployment: {
        programId: SETTLEMENT_PROGRAM_ID,
        cluster: 'mainnet-beta',
        rpcUrl: 'https://api.mainnet-beta.solana.com',
      },
    };
    expect(() => prepareFundComputeJobInstruction(input)).toThrow(/not deployed/);
  });

  it('rejects an RPC URL that serves a different cluster than the one declared', () => {
    const withRpc = (cluster: string, rpcUrl: string) => ({
      ...fundInput(),
      deployment: { programId: PROGRAM_ID, cluster, rpcUrl },
    });

    expect(() =>
      prepareFundComputeJobInstruction(withRpc('devnet', 'https://api.mainnet-beta.solana.com')),
    ).toThrow(/serves mainnet, not the declared cluster devnet/);
    expect(() =>
      prepareFundComputeJobInstruction(withRpc('localnet', 'https://devnet.helius-rpc.com')),
    ).toThrow(/serves devnet/);
    expect(() => prepareFundComputeJobInstruction(withRpc('devnet', 'not-a-url'))).toThrow(
      /absolute URL/,
    );
    expect(() => prepareFundComputeJobInstruction(withRpc('staging', 'http://127.0.0.1:8899'))).toThrow(
      /unknown compute cluster/,
    );
    expect(() =>
      prepareFundComputeJobInstruction(withRpc('devnet', 'https://api.devnet.solana.com')),
    ).not.toThrow();
    // A private endpoint names no cluster; the builder cannot second-guess it.
    expect(() =>
      prepareFundComputeJobInstruction(withRpc('devnet', 'https://rpc.example.com/abc')),
    ).not.toThrow();
  });

  it('defaults the token program to Tokenkeg and accepts Token-2022', () => {
    expect(only(prepareFundComputeJobInstruction(fundInput())).accounts.at(-2)).toMatchObject({
      name: 'token_program',
      address: TOKEN_PROGRAM_ID,
    });
    const t22 = prepareFundComputeJobInstruction({
      ...fundInput(),
      tokenProgram: TOKEN_2022_PROGRAM_ID,
    });
    expect(only(t22).accounts.at(-2)).toMatchObject({
      name: 'token_program',
      address: TOKEN_2022_PROGRAM_ID,
    });
  });

  it('binds the funding signer, token accounts, and immutable quote fields', () => {
    const bundle = prepareFundComputeJobInstruction(fundInput());
    const instruction = only(bundle);

    expect(bundle).toMatchObject({
      chain: 'solana',
      cluster: 'localnet',
      rpcUrl: 'http://127.0.0.1:8899',
    });
    expect(instruction.programId).toBe(PROGRAM_ID);
    expect(instruction.accounts).toEqual([
      { name: 'config', address: SYSTEM_PROGRAM_ID, signer: false, writable: false },
      { name: 'compute_config', address: SYSTEM_PROGRAM_ID, signer: false, writable: false },
      { name: 'escrow', address: SYSTEM_PROGRAM_ID, signer: false, writable: true },
      { name: 'client', address: SYSTEM_PROGRAM_ID, signer: true, writable: true },
      { name: 'client_usdc', address: SYSTEM_PROGRAM_ID, signer: false, writable: true },
      { name: 'provider_usdc', address: SYSTEM_PROGRAM_ID, signer: false, writable: false },
      { name: 'escrow_vault', address: SYSTEM_PROGRAM_ID, signer: false, writable: true },
      { name: 'usdc_mint', address: SYSTEM_PROGRAM_ID, signer: false, writable: false },
      { name: 'token_program', address: TOKEN_PROGRAM_ID, signer: false, writable: false },
      { name: 'system_program', address: SYSTEM_PROGRAM_ID, signer: false, writable: false },
    ]);
    expect(instruction.data).toEqual({
      job_id: JOB_ID,
      quote_commitment: QUOTE_COMMITMENT,
      provider: SYSTEM_PROGRAM_ID,
      max_usdc_amount: '1',
      expires_at: '2',
    });
  });
});

describe('compute settlement serialization', () => {
  it('serializes config initialization and authority rotation from the staged IDL', () => {
    const initialize = toTransactionInstructions(
      prepareInitializeComputePaymentsInstruction({
        deployment,
        configAccount: SYSTEM_PROGRAM_ID,
        computeConfigAccount: SYSTEM_PROGRAM_ID,
        authority: SYSTEM_PROGRAM_ID,
        usdcMint: SYSTEM_PROGRAM_ID,
        settlementAuthority: SYSTEM_PROGRAM_ID,
      }),
    )[0]!;
    const update = toTransactionInstructions(
      prepareUpdateComputeSettlementAuthorityInstruction({
        deployment,
        configAccount: SYSTEM_PROGRAM_ID,
        computeConfigAccount: SYSTEM_PROGRAM_ID,
        authority: SYSTEM_PROGRAM_ID,
        settlementAuthority: SYSTEM_PROGRAM_ID,
      }),
    )[0]!;

    expect(hex(initialize.data)).toBe('cb176aa538284936' + '00'.repeat(32));
    expect(initialize.keys[2]).toMatchObject({ isSigner: true, isWritable: true });
    expect(hex(update.data)).toBe('b9c27928abbce885' + '00'.repeat(32));
    expect(update.keys[2]).toMatchObject({ isSigner: true, isWritable: false });
  });

  it('serializes fund, settle, and refund data in Anchor field order', () => {
    const fund = toTransactionInstructions(prepareFundComputeJobInstruction(fundInput()))[0]!;
    const settle = toTransactionInstructions(
      prepareSettleComputeJobInstruction({
        deployment,
        configAccount: SYSTEM_PROGRAM_ID,
        computeConfigAccount: SYSTEM_PROGRAM_ID,
        escrowAccount: SYSTEM_PROGRAM_ID,
        settlementAuthority: SYSTEM_PROGRAM_ID,
        escrowVault: SYSTEM_PROGRAM_ID,
        providerUsdcAccount: SYSTEM_PROGRAM_ID,
        clientUsdcAccount: SYSTEM_PROGRAM_ID,
        usdcMint: SYSTEM_PROGRAM_ID,
        actualUsdcAmount: '7',
        receiptCommitment: RECEIPT_COMMITMENT,
      }),
    )[0]!;
    const refund = toTransactionInstructions(
      prepareRefundComputeJobInstruction({
        deployment,
        configAccount: SYSTEM_PROGRAM_ID,
        computeConfigAccount: SYSTEM_PROGRAM_ID,
        escrowAccount: SYSTEM_PROGRAM_ID,
        authority: SYSTEM_PROGRAM_ID,
        escrowVault: SYSTEM_PROGRAM_ID,
        clientUsdcAccount: SYSTEM_PROGRAM_ID,
        usdcMint: SYSTEM_PROGRAM_ID,
        refundCommitment: REFUND_COMMITMENT,
      }),
    )[0]!;

    expect(hex(fund.data)).toBe(
      '51c06057e6bd107b' +
        JOB_ID +
        QUOTE_COMMITMENT +
        '00'.repeat(32) +
        '0100000000000000' +
        '0200000000000000',
    );
    expect(hex(settle.data)).toBe('f12fb0e2e2b32d90' + '0700000000000000' + RECEIPT_COMMITMENT);
    expect(settle.keys[3]).toMatchObject({ isSigner: true, isWritable: false });
    expect(hex(refund.data)).toBe('d20a7d82eaa40bb5' + REFUND_COMMITMENT);
    expect(refund.keys[3]).toMatchObject({ isSigner: true, isWritable: false });
  });
});

describe('compute account decoding', () => {
  const u8 = (n: number) => Uint8Array.of(n);

  it('reads back an escrow the client funded through the SDK', () => {
    const data = concat([
      Uint8Array.from([56, 57, 151, 207, 152, 81, 212, 113]),
      hash32(JOB_ID),
      hash32(QUOTE_COMMITMENT),
      pubkey(CLIENT),
      pubkey(TOKEN_PROGRAM_ID),
      pubkey(SYSTEM_PROGRAM_ID),
      pubkey(SYSTEM_PROGRAM_ID),
      pubkey(SYSTEM_PROGRAM_ID),
      pubkey(SYSTEM_PROGRAM_ID),
      u64('1000000'),
      u64('400000'),
      u64('600000'),
      i64('1780000000'),
      i64('1779000000'),
      i64('1779500000'),
      hash32(RECEIPT_COMMITMENT),
      pubkey(CLIENT),
      u8(2),
      u8(255),
    ]);

    const escrow = decodeComputeEscrow(data);
    expect(escrow.jobId).toBe(JOB_ID);
    expect(escrow.client).toBe(CLIENT);
    expect(escrow.provider).toBe(TOKEN_PROGRAM_ID);
    expect(escrow.maxUsdcAmount).toBe(1000000n);
    expect(escrow.actualUsdcAmount + escrow.refundedUsdcAmount).toBe(escrow.maxUsdcAmount);
    expect(escrow.expiresAt).toBe(1780000000n);
    expect(escrow.terminalCommitment).toBe(RECEIPT_COMMITMENT);
    expect(escrow.status).toBe(2);
  });

  it('reads back the payment config and rejects the wrong account type', () => {
    const data = concat([
      Uint8Array.from([199, 106, 161, 139, 149, 124, 183, 244]),
      pubkey(CLIENT),
      pubkey(SYSTEM_PROGRAM_ID),
      u8(254),
    ]);

    const config = decodeComputePaymentConfig(data);
    expect(config.usdcMint).toBe(CLIENT);
    expect(config.settlementAuthority).toBe(SYSTEM_PROGRAM_ID);
    expect(config.bump).toBe(254);

    expect(() => decodeComputeEscrow(data)).toThrow(/discriminator/);
    expect(() => decodeComputePaymentConfig(concat([data, u8(0)]))).toThrow(/trailing bytes/);
  });
});
