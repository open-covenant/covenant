import { describe, expect, it } from 'vitest';
import { covenantBrand } from '@covenant/config/brand';
import {
  hash32FromText,
  prepareAnchorReceiptBatchInstruction,
  prepareBuyCreditsInstruction,
  prepareCreateTaskInstruction,
  prepareRegisterAgentInstruction,
  prepareReleaseTaskInstruction,
  prepareStakeInstruction,
} from '../solana/instructions.js';

const SYSTEM_PROGRAM = '11111111111111111111111111111111';
const TOKEN_PROGRAM = 'TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA';

// Distinct valid base58 addresses per account so a mutation that routes the
// wrong input into a meta() slot is caught alongside flag flips and reorders.
// A single base58 char repeated 32 times satisfies SOLANA_ADDRESS_REGEX.
const addr = (c: string) => c.repeat(32);

const only = (bundle: ReturnType<typeof prepareRegisterAgentInstruction>) => {
  expect(bundle.chain).toBe('solana');
  expect(bundle.instructions).toHaveLength(1);
  return bundle.instructions[0]!;
};

describe('Solana instruction descriptors', () => {
  it('register_agent: account authorization flags and hashed data bindings', () => {
    const ix = only(
      prepareRegisterAgentInstruction({
        configAccount: addr('2'),
        agentAccount: addr('3'),
        operator: addr('4'),
        agentKey: hash32FromText('agent-key'),
        metadataHash: hash32FromText('metadata'),
        capabilityHash: hash32FromText('capabilities'),
      }),
    );

    expect(ix.instruction).toBe('register_agent');
    expect(ix.accounts).toEqual([
      { name: 'config', address: addr('2'), signer: false, writable: false },
      { name: 'agent', address: addr('3'), signer: false, writable: true },
      { name: 'operator', address: addr('4'), signer: true, writable: true },
      { name: 'system_program', address: SYSTEM_PROGRAM, signer: false, writable: false },
    ]);
    expect(ix.data).toEqual({
      agent_key: hash32FromText('agent-key'),
      metadata_hash: hash32FromText('metadata'),
      capability_hash: hash32FromText('capabilities'),
    });
  });

  it('stake: owner is the sole signer and the brand token symbol is bound', () => {
    const ix = only(
      prepareStakeInstruction({
        configAccount: addr('2'),
        agentAccount: addr('3'),
        positionAccount: addr('4'),
        owner: addr('5'),
        ownerCovntAccount: addr('6'),
        stakeVault: addr('7'),
        amountCovnt: '1000',
        lockUntil: '1750000000',
      }),
    );

    expect(ix.instruction).toBe('stake');
    expect(ix.accounts).toEqual([
      { name: 'config', address: addr('2'), signer: false, writable: false },
      { name: 'agent', address: addr('3'), signer: false, writable: true },
      { name: 'position', address: addr('4'), signer: false, writable: true },
      { name: 'owner', address: addr('5'), signer: true, writable: true },
      { name: 'owner_covnt', address: addr('6'), signer: false, writable: true },
      { name: 'stake_vault', address: addr('7'), signer: false, writable: true },
      { name: 'token_program', address: TOKEN_PROGRAM, signer: false, writable: false },
      { name: 'system_program', address: SYSTEM_PROGRAM, signer: false, writable: false },
    ]);
    expect(ix.data).toEqual({
      amount_covnt: '1000',
      lock_until: '1750000000',
      token_symbol: covenantBrand.token.symbol,
    });
  });

  it('buy_credits: owner signs and treasury is writable', () => {
    const ix = only(
      prepareBuyCreditsInstruction({
        configAccount: addr('2'),
        creditAccount: addr('3'),
        owner: addr('4'),
        ownerCovntAccount: addr('5'),
        treasury: addr('6'),
        amountCovnt: '42',
      }),
    );

    expect(ix.instruction).toBe('buy_credits');
    expect(ix.accounts).toEqual([
      { name: 'config', address: addr('2'), signer: false, writable: false },
      { name: 'credits', address: addr('3'), signer: false, writable: true },
      { name: 'owner', address: addr('4'), signer: true, writable: true },
      { name: 'owner_covnt', address: addr('5'), signer: false, writable: true },
      { name: 'treasury', address: addr('6'), signer: false, writable: true },
      { name: 'token_program', address: TOKEN_PROGRAM, signer: false, writable: false },
    ]);
    expect(ix.data).toEqual({ amount_covnt: '42' });
  });

  it('create_task: client signs, agent stays read-only, and task fields bind in order', () => {
    const ix = only(
      prepareCreateTaskInstruction({
        configAccount: addr('2'),
        agentAccount: addr('3'),
        taskAccount: addr('4'),
        client: addr('5'),
        clientCovntAccount: addr('6'),
        escrowVault: addr('7'),
        provider: addr('8'),
        taskId: hash32FromText('task-id'),
        amountCovnt: '500',
        taskHash: hash32FromText('task-hash'),
        criteriaHash: hash32FromText('criteria'),
        deadline: '1760000000',
      }),
    );

    expect(ix.instruction).toBe('create_task');
    expect(ix.accounts).toEqual([
      { name: 'config', address: addr('2'), signer: false, writable: false },
      { name: 'agent', address: addr('3'), signer: false, writable: false },
      { name: 'task', address: addr('4'), signer: false, writable: true },
      { name: 'client', address: addr('5'), signer: true, writable: true },
      { name: 'client_covnt', address: addr('6'), signer: false, writable: true },
      { name: 'escrow_vault', address: addr('7'), signer: false, writable: true },
      { name: 'token_program', address: TOKEN_PROGRAM, signer: false, writable: false },
      { name: 'system_program', address: SYSTEM_PROGRAM, signer: false, writable: false },
    ]);
    expect(ix.data).toEqual({
      provider: addr('8'),
      task_id: hash32FromText('task-id'),
      amount_covnt: '500',
      task_hash: hash32FromText('task-hash'),
      criteria_hash: hash32FromText('criteria'),
      deadline: '1760000000',
    });
  });

  it('release_task: client signs but is not writable, and result/receipt hashes do not transpose', () => {
    const ix = only(
      prepareReleaseTaskInstruction({
        configAccount: addr('2'),
        taskAccount: addr('3'),
        client: addr('4'),
        escrowVault: addr('5'),
        providerCovntAccount: addr('6'),
        resultHash: hash32FromText('result'),
        receiptHash: hash32FromText('receipt'),
      }),
    );

    expect(ix.instruction).toBe('release_task');
    expect(ix.accounts).toEqual([
      { name: 'config', address: addr('2'), signer: false, writable: false },
      { name: 'task', address: addr('3'), signer: false, writable: true },
      { name: 'client', address: addr('4'), signer: true, writable: false },
      { name: 'escrow_vault', address: addr('5'), signer: false, writable: true },
      { name: 'provider_covnt', address: addr('6'), signer: false, writable: true },
      { name: 'token_program', address: TOKEN_PROGRAM, signer: false, writable: false },
    ]);
    expect(ix.data).toEqual({
      result_hash: hash32FromText('result'),
      receipt_hash: hash32FromText('receipt'),
    });
    expect(ix.data.result_hash).not.toBe(ix.data.receipt_hash);
  });

  it('anchor_receipt_batch: authority signs and the receipt count is preserved', () => {
    const ix = only(
      prepareAnchorReceiptBatchInstruction({
        configAccount: addr('2'),
        batchAccount: addr('3'),
        authority: addr('4'),
        batchId: hash32FromText('batch'),
        merkleRoot: hash32FromText('root'),
        receiptCount: 2,
      }),
    );

    expect(ix.instruction).toBe('anchor_receipt_batch');
    expect(ix.accounts).toEqual([
      { name: 'config', address: addr('2'), signer: false, writable: false },
      { name: 'batch', address: addr('3'), signer: false, writable: true },
      { name: 'authority', address: addr('4'), signer: true, writable: true },
      { name: 'system_program', address: SYSTEM_PROGRAM, signer: false, writable: false },
    ]);
    expect(ix.data).toEqual({
      batch_id: hash32FromText('batch'),
      merkle_root: hash32FromText('root'),
      receipt_count: 2,
    });
  });
});
