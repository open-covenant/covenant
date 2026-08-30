import { sha256 } from '@noble/hashes/sha256';
import { bytesToHex } from '@noble/hashes/utils';
import { COVENANT_TOKEN_SYMBOL, DEFAULT_PROTOCOL_PROGRAM_ID } from '../config.js';
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
  taskId: Hash32;
  amountCovnt: string;
  taskHash: Hash32;
  criteriaHash: Hash32;
  deadline: string;
}

export interface ReleaseTaskInput {
  configAccount: SolanaAddress;
  client: SolanaAddress;
  taskAccount: SolanaAddress;
  escrowVault: SolanaAddress;
  providerCovntAccount: SolanaAddress;
  covntMint: SolanaAddress;
  resultHash: Hash32;
  receiptHash: Hash32;
}

export interface AnchorReceiptBatchInput {
  configAccount: SolanaAddress;
  authority: SolanaAddress;
  batchAccount: SolanaAddress;
  batchId: Hash32;
  merkleRoot: Hash32;
  receiptCount: number;
}

export interface ComputeProgramDeployment {
  programId: SolanaAddress;
  cluster: string;
  rpcUrl: string;
}

interface ComputeInstructionInput {
  deployment: ComputeProgramDeployment;
  configAccount: SolanaAddress;
  computeConfigAccount: SolanaAddress;
}

// The program takes any SPL token interface; Tokenkeg is only the default.
interface ComputeTokenInput {
  tokenProgram?: SolanaAddress;
}

export interface InitializeComputePaymentsInput extends ComputeInstructionInput {
  authority: SolanaAddress;
  usdcMint: SolanaAddress;
  settlementAuthority: SolanaAddress;
}

export interface UpdateComputeSettlementAuthorityInput extends ComputeInstructionInput {
  authority: SolanaAddress;
  settlementAuthority: SolanaAddress;
}

export interface FundComputeJobInput extends ComputeInstructionInput, ComputeTokenInput {
  escrowAccount: SolanaAddress;
  client: SolanaAddress;
  clientUsdcAccount: SolanaAddress;
  providerUsdcAccount: SolanaAddress;
  escrowVault: SolanaAddress;
  usdcMint: SolanaAddress;
  jobId: Hash32;
  quoteCommitment: Hash32;
  provider: SolanaAddress;
  maxUsdcAmount: string;
  expiresAt: string;
}

export interface SettleComputeJobInput extends ComputeInstructionInput, ComputeTokenInput {
  escrowAccount: SolanaAddress;
  settlementAuthority: SolanaAddress;
  escrowVault: SolanaAddress;
  providerUsdcAccount: SolanaAddress;
  clientUsdcAccount: SolanaAddress;
  usdcMint: SolanaAddress;
  actualUsdcAmount: string;
  receiptCommitment: Hash32;
}

export interface RefundComputeJobInput extends ComputeInstructionInput, ComputeTokenInput {
  escrowAccount: SolanaAddress;
  authority: SolanaAddress;
  escrowVault: SolanaAddress;
  clientUsdcAccount: SolanaAddress;
  usdcMint: SolanaAddress;
  refundCommitment: Hash32;
}

const SYSTEM_PROGRAM_ID = '11111111111111111111111111111111';
const TOKEN_PROGRAM_ID = 'TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA';

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
      provider: assertSolanaAddress(input.provider, 'provider'),
      task_id: assertHash32(input.taskId, 'task id'),
      amount_covnt: input.amountCovnt,
      task_hash: assertHash32(input.taskHash, 'task hash'),
      criteria_hash: assertHash32(input.criteriaHash, 'criteria hash'),
      deadline: input.deadline,
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
      meta('client', input.client, true, false),
      meta('escrow_vault', input.escrowVault, false, true),
      meta('provider_covnt', input.providerCovntAccount, false, true),
      meta('covnt_mint', input.covntMint, false, false),
      meta('token_program', TOKEN_PROGRAM_ID, false, false),
    ],
    data: {
      result_hash: assertHash32(input.resultHash, 'result hash'),
      receipt_hash: assertHash32(input.receiptHash, 'receipt hash'),
    },
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

export function prepareInitializeComputePaymentsInstruction(
  input: InitializeComputePaymentsInput,
): PreparedSolanaBundle {
  return computeBundle(input.deployment, {
    instruction: 'initialize_compute_payments',
    accounts: [
      meta('config', input.configAccount, false, false),
      meta('compute_config', input.computeConfigAccount, false, true),
      meta('authority', input.authority, true, true),
      meta('usdc_mint', input.usdcMint, false, false),
      meta('system_program', SYSTEM_PROGRAM_ID, false, false),
    ],
    data: {
      settlement_authority: assertSolanaAddress(input.settlementAuthority, 'settlement authority'),
    },
  });
}

export function prepareUpdateComputeSettlementAuthorityInstruction(
  input: UpdateComputeSettlementAuthorityInput,
): PreparedSolanaBundle {
  return computeBundle(input.deployment, {
    instruction: 'update_compute_settlement_authority',
    accounts: [
      meta('config', input.configAccount, false, false),
      meta('compute_config', input.computeConfigAccount, false, true),
      meta('authority', input.authority, true, false),
    ],
    data: {
      settlement_authority: assertSolanaAddress(input.settlementAuthority, 'settlement authority'),
    },
  });
}

export function prepareFundComputeJobInstruction(input: FundComputeJobInput): PreparedSolanaBundle {
  return computeBundle(input.deployment, {
    instruction: 'fund_compute_job',
    accounts: [
      meta('config', input.configAccount, false, false),
      meta('compute_config', input.computeConfigAccount, false, false),
      meta('escrow', input.escrowAccount, false, true),
      meta('client', input.client, true, true),
      meta('client_usdc', input.clientUsdcAccount, false, true),
      meta('provider_usdc', input.providerUsdcAccount, false, false),
      meta('escrow_vault', input.escrowVault, false, true),
      meta('usdc_mint', input.usdcMint, false, false),
      meta('token_program', input.tokenProgram ?? TOKEN_PROGRAM_ID, false, false),
      meta('system_program', SYSTEM_PROGRAM_ID, false, false),
    ],
    data: {
      job_id: assertHash32(input.jobId, 'job id'),
      quote_commitment: assertHash32(input.quoteCommitment, 'quote commitment'),
      provider: assertSolanaAddress(input.provider, 'provider'),
      max_usdc_amount: input.maxUsdcAmount,
      expires_at: input.expiresAt,
    },
  });
}

export function prepareSettleComputeJobInstruction(
  input: SettleComputeJobInput,
): PreparedSolanaBundle {
  return computeBundle(input.deployment, {
    instruction: 'settle_compute_job',
    accounts: [
      meta('config', input.configAccount, false, false),
      meta('compute_config', input.computeConfigAccount, false, false),
      meta('escrow', input.escrowAccount, false, true),
      meta('settlement_authority', input.settlementAuthority, true, false),
      meta('escrow_vault', input.escrowVault, false, true),
      meta('provider_usdc', input.providerUsdcAccount, false, true),
      meta('client_usdc', input.clientUsdcAccount, false, true),
      meta('usdc_mint', input.usdcMint, false, false),
      meta('token_program', input.tokenProgram ?? TOKEN_PROGRAM_ID, false, false),
    ],
    data: {
      actual_usdc_amount: input.actualUsdcAmount,
      receipt_commitment: assertHash32(input.receiptCommitment, 'receipt commitment'),
    },
  });
}

export function prepareRefundComputeJobInstruction(
  input: RefundComputeJobInput,
): PreparedSolanaBundle {
  return computeBundle(input.deployment, {
    instruction: 'refund_compute_job',
    accounts: [
      meta('config', input.configAccount, false, false),
      meta('compute_config', input.computeConfigAccount, false, false),
      meta('escrow', input.escrowAccount, false, true),
      meta('authority', input.authority, true, false),
      meta('escrow_vault', input.escrowVault, false, true),
      meta('client_usdc', input.clientUsdcAccount, false, true),
      meta('usdc_mint', input.usdcMint, false, false),
      meta('token_program', input.tokenProgram ?? TOKEN_PROGRAM_ID, false, false),
    ],
    data: {
      refund_commitment: assertHash32(input.refundCommitment, 'refund commitment'),
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

const CLUSTER_ALIASES: Record<string, string> = {
  mainnet: 'mainnet',
  'mainnet-beta': 'mainnet',
  devnet: 'devnet',
  testnet: 'testnet',
  localnet: 'localnet',
  localhost: 'localnet',
};

const LOCAL_HOSTS = new Set(['localhost', '127.0.0.1', '0.0.0.0', '::1', '[::1]']);
const CLUSTER_HOSTS: Array<[string, RegExp]> = [
  ['mainnet', /(^|[.-])mainnet([.-]|$)/],
  ['devnet', /(^|[.-])devnet([.-]|$)/],
  ['testnet', /(^|[.-])testnet([.-]|$)/],
];

// The cluster a host advertises, or null when it keeps that private (a
// dedicated RPC on a vanity domain). Naming is the only signal available
// without a getGenesisHash round trip, which a builder must stay free of.
function hostCluster(host: string): string | null {
  if (LOCAL_HOSTS.has(host)) return 'localnet';
  return CLUSTER_HOSTS.find(([, pattern]) => pattern.test(host))?.[0] ?? null;
}

function computeBundle(
  deployment: ComputeProgramDeployment,
  instruction: Omit<PreparedSolanaInstruction, 'programId'>,
): PreparedSolanaBundle {
  if (!deployment) throw new Error('an explicit compute deployment is required');
  const programId = assertSolanaAddress(deployment.programId, 'compute program id');
  const cluster = deployment.cluster.trim();
  const rpcUrl = deployment.rpcUrl.trim();
  if (!cluster) throw new Error('compute cluster is required');
  if (!rpcUrl) throw new Error('compute RPC URL is required');

  const declared = CLUSTER_ALIASES[cluster.toLowerCase()];
  if (!declared) throw new Error(`unknown compute cluster: ${cluster}`);
  let host: string;
  try {
    host = new URL(rpcUrl).hostname.toLowerCase();
  } catch {
    throw new Error(`compute RPC URL must be an absolute URL, got: ${rpcUrl}`);
  }
  const actual = hostCluster(host);
  if (actual && actual !== declared) {
    throw new Error(`compute RPC URL ${host} serves ${actual}, not the declared cluster ${cluster}`);
  }
  if (programId === DEFAULT_PROTOCOL_PROGRAM_ID && declared === 'mainnet') {
    throw new Error('compute settlement is not deployed at the current mainnet program');
  }

  return {
    chain: 'solana',
    cluster,
    rpcUrl,
    instructions: [{ ...instruction, programId }],
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
