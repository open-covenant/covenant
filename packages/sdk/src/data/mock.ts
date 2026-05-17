import { covenantBrand } from '@covenant/config/brand';
import type { SolanaAddress, Hash32 } from '../solana/accounts.js';
import { hash32FromText } from '../solana/instructions.js';
import type { TaskStatus } from '../domain/task.js';

export interface ReputationDims {
  execution: number;
  receipts: number;
  stake: number;
  provenance: number;
}

export interface AgentSummary {
  agentId: Hash32;
  operatorAddress: SolanaAddress;
  metadataUri: string;
  capabilityHash: Hash32;
  stakeAmount: string;
  reputationScore: number;
  active: boolean;
  tags: string[];
}

export interface AgentDetail extends AgentSummary {
  taskCount: number;
  creditBalance: string;
  reputationDims: ReputationDims;
}

export interface TaskSummary {
  taskId: Hash32;
  agentId: Hash32;
  clientAddress: SolanaAddress;
  paymentMint: SolanaAddress;
  paymentAmount: string;
  paymentSymbol: string;
  status: TaskStatus;
  signature: string;
  programId: SolanaAddress;
  deadline: string;
}

export interface TaskDetail extends TaskSummary {
  description: string;
  criteriaHash: Hash32;
  receiptHash: Hash32;
  resultHash: Hash32;
}

export interface CreditSummary {
  owner: SolanaAddress;
  balance: string;
  lastBatchRoot: Hash32;
}

export interface LeaderboardRow {
  agentId: Hash32;
  operatorAddress: SolanaAddress;
  score: number;
  completedTasks: number;
  stakeAmount: string;
}

export interface MarketplaceBounty {
  taskHash: Hash32;
  title: string;
  bounty: string;
}

const PROGRAM_ID = 'CovntSettLement1111111111111111111111111111';
const COVNT_MINT = 'CovntMint111111111111111111111111111111111';
const ALPHA_OPERATOR = 'Alpha111111111111111111111111111111111111';
const DELTA_OPERATOR = 'Delta111111111111111111111111111111111111';
const CLIENT = 'Client11111111111111111111111111111111111';

export const MOCK_AGENTS: AgentSummary[] = [
  {
    agentId: hash32FromText('covenant.alpha'),
    operatorAddress: ALPHA_OPERATOR,
    metadataUri: 'https://opencovenant.org/agents/alpha.json',
    capabilityHash: hash32FromText('execution,receipts,stake'),
    stakeAmount: '2500000000',
    reputationScore: 962,
    active: true,
    tags: ['execution', 'receipts', 'stake'],
  },
  {
    agentId: hash32FromText('covenant.delta'),
    operatorAddress: DELTA_OPERATOR,
    metadataUri: 'https://opencovenant.org/agents/delta.json',
    capabilityHash: hash32FromText('routing,provenance'),
    stakeAmount: '1800000000',
    reputationScore: 914,
    active: true,
    tags: ['routing', 'provenance'],
  },
];

export const MOCK_AGENT_DETAILS: AgentDetail[] = MOCK_AGENTS.map((agent, index) => ({
  ...agent,
  taskCount: 12 - index * 3,
  creditBalance: `${90000 - index * 12000}`,
  reputationDims: {
    execution: 92 - index * 3,
    receipts: 95 - index * 2,
    stake: 88 - index,
    provenance: 84 + index,
  },
}));

export const MOCK_TASKS: TaskDetail[] = [
  {
    taskId: hash32FromText('task.solana.bootstrap'),
    agentId: MOCK_AGENTS[0]!.agentId,
    clientAddress: CLIENT,
    paymentMint: COVNT_MINT,
    paymentAmount: '125000000',
    paymentSymbol: covenantBrand.token.symbol,
    status: 'verified',
    signature: '5uA7rQ9mZQ7tJ4o8h4q9LkT7o6r8mQ2p5z6x7c8v9b1n2m3q4w5e6r7t8y9u',
    programId: PROGRAM_ID,
    deadline: new Date(Date.now() + 48 * 3600_000).toISOString(),
    description: 'Ship the Solana receipt anchoring healthcheck.',
    criteriaHash: hash32FromText('criteria.bootstrap'),
    receiptHash: hash32FromText('receipt.bootstrap'),
    resultHash: hash32FromText('result.bootstrap'),
  },
  {
    taskId: hash32FromText('task.solana.settlement'),
    agentId: MOCK_AGENTS[1]!.agentId,
    clientAddress: CLIENT,
    paymentMint: COVNT_MINT,
    paymentAmount: '84000000',
    paymentSymbol: covenantBrand.token.symbol,
    status: 'funded',
    signature: '3mQ7rQ9mZQ7tJ4o8h4q9LkT7o6r8mQ2p5z6x7c8v9b1n2m3q4w5e6r7t8y',
    programId: PROGRAM_ID,
    deadline: new Date(Date.now() + 72 * 3600_000).toISOString(),
    description: 'Publish COVNT credit burn and stake digest.',
    criteriaHash: hash32FromText('criteria.settlement'),
    receiptHash: hash32FromText('receipt.settlement'),
    resultHash: hash32FromText('result.settlement'),
  },
];

export const MOCK_CREDITS: CreditSummary[] = [
  {
    owner: ALPHA_OPERATOR,
    balance: '540000',
    lastBatchRoot: hash32FromText('batch.alpha'),
  },
];

export const MOCK_LEADERBOARD: LeaderboardRow[] = MOCK_AGENT_DETAILS.map((agent, index) => ({
  agentId: agent.agentId,
  operatorAddress: agent.operatorAddress,
  score: agent.reputationScore,
  completedTasks: 14 - index * 2,
  stakeAmount: agent.stakeAmount,
}));

export const MARKETPLACE_BOUNTIES: MarketplaceBounty[] = MOCK_TASKS.map((task) => ({
  taskHash: hash32FromText(task.description),
  title: task.description,
  bounty: task.paymentAmount,
}));

export async function fetchCredits(owner: SolanaAddress): Promise<CreditSummary | null> {
  return MOCK_CREDITS.find((item) => item.owner === owner) ?? null;
}

export function findMarketplaceBountyByTaskHash(taskHash: Hash32) {
  return MARKETPLACE_BOUNTIES.find((item) => item.taskHash === taskHash) ?? null;
}
