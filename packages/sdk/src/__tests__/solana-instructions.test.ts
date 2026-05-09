import { describe, expect, it } from 'vitest';
import {
  hash32FromText,
  prepareAnchorReceiptBatchInstruction,
  prepareRegisterAgentInstruction,
} from '../solana/instructions.js';

const ADDRESS = '11111111111111111111111111111111';

describe('Solana instruction descriptors', () => {
  it('prepares register-agent instructions with Solana accounts', () => {
    const bundle = prepareRegisterAgentInstruction({
      configAccount: ADDRESS,
      operator: ADDRESS,
      agentAccount: ADDRESS,
      agentKey: hash32FromText('agent'),
      metadataHash: hash32FromText('metadata'),
      capabilityHash: hash32FromText('capabilities'),
    });

    expect(bundle.chain).toBe('solana');
    expect(bundle.instructions[0]!.instruction).toBe('register_agent');
    expect(bundle.instructions[0]!.accounts.map((account) => account.name)).toEqual([
      'config',
      'agent',
      'operator',
      'system_program',
    ]);
  });

  it('prepares receipt batch anchoring descriptors', () => {
    const bundle = prepareAnchorReceiptBatchInstruction({
      configAccount: ADDRESS,
      authority: ADDRESS,
      batchAccount: ADDRESS,
      batchId: hash32FromText('batch'),
      merkleRoot: hash32FromText('root'),
      receiptCount: 2,
    });

    expect(bundle.instructions[0]!.instruction).toBe('anchor_receipt_batch');
    expect(bundle.instructions[0]!.data.receipt_count).toBe(2);
    expect(bundle.instructions[0]!.accounts.at(-1)?.name).toBe('system_program');
  });
});
