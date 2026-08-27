import { describe, expect, it } from 'vitest';
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
    const escrow = deriveComputeEscrowPda('22'.repeat(32), PROGRAM_ID);

    expect(config.address.toBase58()).toBe('ER3t6Cxz5AQ3cS8nJGPyj5PpGpui1kRMchr7wn1BsGuM');
    expect(config.bump).toBe(255);
    expect(escrow.address.toBase58()).toBe('8HPCUk8nZHjhQCuaTe3H3cEjuvS7pYK5uQpCAiaUXRLH');
    expect(escrow.bump).toBe(254);
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
