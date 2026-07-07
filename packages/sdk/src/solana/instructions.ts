import { sha256 } from '@noble/hashes/sha256';
import { bytesToHex } from '@noble/hashes/utils';
import { COVENANT_TOKEN_SYMBOL } from '../config.js';
import {
  assertHash32,
  assertSolanaAddress,
  type Hash32,
  type SolanaAddress,
} from './accounts.js';
import { resolveSolanaNetwork } from './network.js';

export interface PreparedAccountMeta {
  name: string;
  address: SolanaAddress;
  signer: boolean;
  writable: boolean;
}

export interface PreparedSolanaInstruction {
  programId: SolanaAddress;
  instruction: string;
  accounts: PreparedAccountMeta[];
  data: Record<string, string | number | boolean | null>;
}

export interface PreparedSolanaBundle {
  chain: 'solana';
  cluster: string;
  rpcUrl: string;
  instructions: PreparedSolanaInstruction[];
}

export interface RegisterAgentInput {
  configAccount: SolanaAddress;
  operator: SolanaAddress;
  agentKey: Hash32;
  metadataHash: Hash32;
  capabilityHash: Hash32;
  agentAccount: SolanaAddress;
}

export interface StakeInput {
  configAccount: SolanaAddress;
  owner: SolanaAddress;
  agentAccount: SolanaAddress;
  positionAccount: SolanaAddress;
  ownerCovntAccount: SolanaAddress;
  stakeVault: SolanaAddress;
  covntMint: SolanaAddress;
  amountCovnt: string;
  lockUntil: string;
}

export interface BuyCreditsInput {
  configAccount: SolanaAddress;
  owner: SolanaAddress;
  creditAccount: SolanaAddress;
  ownerCovntAccount: SolanaAddress;
  treasury: SolanaAddress;
  covntMint: SolanaAddress;
  amountCovnt: string;
}

export interface CreateTaskInput {
  configAccount: SolanaAddress;
  client: SolanaAddress;
  agentAccount: SolanaAddress;
  taskAccount: SolanaAddress;
  clientCovntAccount: SolanaAddress;
  escrowVault: SolanaAddress;
  covntMint: SolanaAddress;
  provider: SolanaAddress;
  /** Optional third-party dispute resolver; omit for a client-only escrow. */
  arbiter?: SolanaAddress;
  taskId: Hash32;
  amountCovnt: string;
  taskHash: Hash32;
  criteriaHash: Hash32;
  deadline: string;
  /** Seconds after submission before the provider may claim unilaterally. */
  reviewWindow: string;
}

export interface SubmitTaskInput {
  configAccount: SolanaAddress;
  taskAccount: SolanaAddress;
  provider: SolanaAddress;
  resultHash: Hash32;
  receiptHash: Hash32;
}

export interface ReleaseTaskInput {
  configAccount: SolanaAddress;
  taskAccount: SolanaAddress;
  authority: SolanaAddress;
  escrowVault: SolanaAddress;
  providerCovntAccount: SolanaAddress;
  covntMint: SolanaAddress;
}

export interface ClaimTaskInput {
  configAccount: SolanaAddress;
  taskAccount: SolanaAddress;
  provider: SolanaAddress;
  escrowVault: SolanaAddress;
  providerCovntAccount: SolanaAddress;
  covntMint: SolanaAddress;
}

export interface RefundTaskInput {
  configAccount: SolanaAddress;
  taskAccount: SolanaAddress;
  authority: SolanaAddress;
  escrowVault: SolanaAddress;
  clientCovntAccount: SolanaAddress;
  covntMint: SolanaAddress;
}

export interface AnchorReceiptBatchInput {
  configAccount: SolanaAddress;
  authority: SolanaAddress;
  batchAccount: SolanaAddress;
  batchId: Hash32;
  merkleRoot: Hash32;
  receiptCount: number;
}

const SYSTEM_PROGRAM_ID = '11111111111111111111111111111111';
const TOKEN_PROGRAM_ID = 'TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA';
const DEFAULT_PUBKEY = '11111111111111111111111111111111';

export function hash32FromText(value: string): Hash32 {
  return bytesToHex(sha256(new TextEncoder().encode(value)));
}

export function prepareRegisterAgentInstruction(input: RegisterAgentInput): PreparedSolanaBundle {
  const network = resolveSolanaNetwork();
  return bundle(network, {
    programId: network.programId,
    instruction: 'register_agent',
    accounts: [
      meta('config', input.configAccount, false, false),
      meta('agent', input.agentAccount, false, true),
      meta('operator', input.operator, true, true),
      meta('system_program', SYSTEM_PROGRAM_ID, false, false),
    ],
    data: {
      agent_key: assertHash32(input.agentKey, 'agent key'),
      metadata_hash: assertHash32(input.metadataHash, 'metadata hash'),
      capability_hash: assertHash32(input.capabilityHash, 'capability hash'),
    },
  });
}

export function prepareStakeInstruction(input: StakeInput): PreparedSolanaBundle {
  const network = resolveSolanaNetwork();
  return bundle(network, {
    programId: network.programId,
    instruction: 'stake',
    accounts: [
      meta('config', input.configAccount, false, false),
      meta('agent', input.agentAccount, false, true),
      meta('position', input.positionAccount, false, true),
      meta('owner', input.owner, true, true),
      meta('owner_covnt', input.ownerCovntAccount, false, true),
      meta('stake_vault', input.stakeVault, false, true),
      meta('covnt_mint', input.covntMint, false, false),
      meta('token_program', TOKEN_PROGRAM_ID, false, false),
      meta('system_program', SYSTEM_PROGRAM_ID, false, false),
    ],
    data: {
      amount_covnt: input.amountCovnt,
      lock_until: input.lockUntil,
      token_symbol: COVENANT_TOKEN_SYMBOL,
    },
  });
}

export function prepareBuyCreditsInstruction(input: BuyCreditsInput): PreparedSolanaBundle {
  const network = resolveSolanaNetwork();
  return bundle(network, {
    programId: network.programId,
    instruction: 'buy_credits',
    accounts: [
      meta('config', input.configAccount, false, false),
      meta('credits', input.creditAccount, false, true),
      meta('owner', input.owner, true, true),
      meta('owner_covnt', input.ownerCovntAccount, false, true),
      meta('treasury', input.treasury, false, true),
      meta('covnt_mint', input.covntMint, false, false),
      meta('token_program', TOKEN_PROGRAM_ID, false, false),
    ],
    data: {
      amount_covnt: input.amountCovnt,
    },
  });
}

export function prepareCreateTaskInstruction(input: CreateTaskInput): PreparedSolanaBundle {
  const network = resolveSolanaNetwork();
  return bundle(network, {
    programId: network.programId,
    instruction: 'create_task',
    accounts: [
      meta('config', input.configAccount, false, false),
      meta('agent', input.agentAccount, false, false),
      meta('task', input.taskAccount, false, true),
      meta('client', input.client, true, true),
      meta('client_covnt', input.clientCovntAccount, false, true),
      meta('escrow_vault', input.escrowVault, false, true),
      meta('covnt_mint', input.covntMint, false, false),
      meta('token_program', TOKEN_PROGRAM_ID, false, false),
      meta('system_program', SYSTEM_PROGRAM_ID, false, false),
    ],
    data: {
      task_id: assertHash32(input.taskId, 'task id'),
      provider: assertSolanaAddress(input.provider, 'provider'),
      arbiter: assertSolanaAddress(input.arbiter ?? DEFAULT_PUBKEY, 'arbiter'),
      amount_covnt: input.amountCovnt,
      task_hash: assertHash32(input.taskHash, 'task hash'),
      criteria_hash: assertHash32(input.criteriaHash, 'criteria hash'),
      deadline: input.deadline,
      review_window: input.reviewWindow,
    },
  });
}

export function prepareSubmitTaskInstruction(input: SubmitTaskInput): PreparedSolanaBundle {
  const network = resolveSolanaNetwork();
  return bundle(network, {
    programId: network.programId,
    instruction: 'submit_task',
    accounts: [
      meta('config', input.configAccount, false, false),
      meta('task', input.taskAccount, false, true),
      meta('provider', input.provider, true, false),
    ],
    data: {
      result_hash: assertHash32(input.resultHash, 'result hash'),
      receipt_hash: assertHash32(input.receiptHash, 'receipt hash'),
    },
  });
}

export function prepareReleaseTaskInstruction(input: ReleaseTaskInput): PreparedSolanaBundle {
  const network = resolveSolanaNetwork();
  return bundle(network, {
    programId: network.programId,
    instruction: 'release_task',
    accounts: [
      meta('config', input.configAccount, false, false),
      meta('task', input.taskAccount, false, true),
      meta('authority', input.authority, true, false),
      meta('escrow_vault', input.escrowVault, false, true),
      meta('provider_covnt', input.providerCovntAccount, false, true),
      meta('covnt_mint', input.covntMint, false, false),
      meta('token_program', TOKEN_PROGRAM_ID, false, false),
    ],
    data: {},
  });
}

export function prepareClaimTaskInstruction(input: ClaimTaskInput): PreparedSolanaBundle {
  const network = resolveSolanaNetwork();
  return bundle(network, {
    programId: network.programId,
    instruction: 'claim_task',
    accounts: [
      meta('config', input.configAccount, false, false),
      meta('task', input.taskAccount, false, true),
      meta('provider', input.provider, true, false),
      meta('escrow_vault', input.escrowVault, false, true),
      meta('provider_covnt', input.providerCovntAccount, false, true),
      meta('covnt_mint', input.covntMint, false, false),
      meta('token_program', TOKEN_PROGRAM_ID, false, false),
    ],
    data: {},
  });
}

export function prepareRefundTaskInstruction(input: RefundTaskInput): PreparedSolanaBundle {
  const network = resolveSolanaNetwork();
  return bundle(network, {
    programId: network.programId,
    instruction: 'refund_task',
    accounts: [
      meta('config', input.configAccount, false, false),
      meta('task', input.taskAccount, false, true),
      meta('authority', input.authority, true, false),
      meta('escrow_vault', input.escrowVault, false, true),
      meta('client_covnt', input.clientCovntAccount, false, true),
      meta('covnt_mint', input.covntMint, false, false),
      meta('token_program', TOKEN_PROGRAM_ID, false, false),
    ],
    data: {},
  });
}

export function prepareAnchorReceiptBatchInstruction(
  input: AnchorReceiptBatchInput,
): PreparedSolanaBundle {
  const network = resolveSolanaNetwork();
  return bundle(network, {
    programId: network.programId,
    instruction: 'anchor_receipt_batch',
    accounts: [
      meta('config', input.configAccount, false, false),
      meta('batch', input.batchAccount, false, true),
      meta('authority', input.authority, true, true),
      meta('system_program', SYSTEM_PROGRAM_ID, false, false),
    ],
    data: {
      batch_id: assertHash32(input.batchId, 'batch id'),
      merkle_root: assertHash32(input.merkleRoot, 'merkle root'),
      receipt_count: input.receiptCount,
    },
  });
}

function bundle(
  network: ReturnType<typeof resolveSolanaNetwork>,
  instruction: PreparedSolanaInstruction,
): PreparedSolanaBundle {
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
