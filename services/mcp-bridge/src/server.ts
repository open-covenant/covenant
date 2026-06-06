import { randomBytes } from 'node:crypto';
import { Server } from '@modelcontextprotocol/sdk/server/index.js';
import { StdioServerTransport } from '@modelcontextprotocol/sdk/server/stdio.js';
import { CallToolRequestSchema, ListToolsRequestSchema } from '@modelcontextprotocol/sdk/types.js';
import type { Tool } from '@modelcontextprotocol/sdk/types.js';
import { z } from 'zod';
import { callDaemonTool, daemonTools } from './daemon.js';
import { buildSolanaProposal, solanaProposeTxTool } from './verifiable.js';

function randomHash32(): string {
  return randomBytes(32).toString('hex');
}
import {
  MOCK_AGENT_DETAILS,
  MOCK_TASKS,
  TASK_STATUS_VALUES,
  hash32FromText,
  prepareAnchorReceiptBatchInstruction,
  prepareBuyCreditsInstruction,
  prepareCreateTaskInstruction,
  prepareRegisterAgentInstruction,
  prepareStakeInstruction,
  resolveSolanaNetwork,
  isSolanaAddress,
} from '@covenant/sdk';

const network = resolveSolanaNetwork();

const server = new Server(
  {
    name: 'covenant',
    version: '0.2.0',
  },
  {
    capabilities: {
      tools: {},
    },
  },
);

const addressSchema = z.string().refine(isSolanaAddress, 'expected a Solana address');
const hash32Schema = z.string().regex(/^[a-f0-9]{64}$/i, 'expected a 32-byte hex string');

const listAgentsSchema = z.object({
  capability: z.string().optional(),
  operatorAddress: addressSchema.optional(),
});

const listTasksSchema = z.object({
  status: z.enum(TASK_STATUS_VALUES).optional(),
  agentId: hash32Schema.optional(),
});

const registerAgentSchema = z.object({
  configAccount: addressSchema,
  operator: addressSchema,
  agentAccount: addressSchema,
  name: z.string().min(1),
  metadataHash: hash32Schema.optional(),
  capabilityHash: hash32Schema.optional(),
});

const buyCreditsSchema = z.object({
  configAccount: addressSchema,
  owner: addressSchema,
  creditAccount: addressSchema,
  ownerCovntAccount: addressSchema,
  treasury: addressSchema,
  amountCovnt: z.string().min(1),
});

const stakeSchema = z.object({
  configAccount: addressSchema,
  owner: addressSchema,
  agentAccount: addressSchema,
  positionAccount: addressSchema,
  ownerCovntAccount: addressSchema,
  stakeVault: addressSchema,
  amountCovnt: z.string().min(1),
  lockUntil: z.string().min(1),
});

const createTaskSchema = z.object({
  configAccount: addressSchema,
  client: addressSchema,
  agentAccount: addressSchema,
  taskAccount: addressSchema,
  clientCovntAccount: addressSchema,
  escrowVault: addressSchema,
  provider: addressSchema,
  description: z.string().min(3),
  amountCovnt: z.string().min(1),
  deadline: z.string().min(1),
  taskId: hash32Schema.optional(),
});

const receiptBatchSchema = z.object({
  configAccount: addressSchema,
  authority: addressSchema,
  batchAccount: addressSchema,
  batchId: hash32Schema,
  merkleRoot: hash32Schema,
  receiptCount: z.number().int().positive(),
});

const deriveIdSchema = z.object({
  value: z.string().min(1),
});

type TextContent = {
  type: 'text';
  text: string;
};

type ToolResult = {
  content: TextContent[];
  isError?: boolean;
};

type ToolInputSchema = Tool['inputSchema'];

function asText(value: unknown): ToolResult {
  return {
    content: [
      {
        type: 'text',
        text: JSON.stringify(value, null, 2),
      },
    ],
  };
}

function asError(message: string): ToolResult {
  return {
    content: [
      {
        type: 'text',
        text: message,
      },
    ],
    isError: true,
  };
}

function describeTool(name: string, description: string, inputSchema: ToolInputSchema): Tool {
  return {
    name,
    description,
    inputSchema,
  };
}

export const tools = [
  describeTool('get_network', 'Return the active Covenant Solana network metadata.', {
    type: 'object',
  }),
  describeTool('list_agents', 'List active Covenant agents using Solana-native identifiers.', {
    type: 'object',
    properties: {
      capability: { type: 'string' },
      operatorAddress: { type: 'string' },
    },
  }),
  describeTool('list_tasks', 'List Covenant tasks using Solana-native identifiers.', {
    type: 'object',
    properties: {
      status: {
        type: 'string',
        enum: [...TASK_STATUS_VALUES],
      },
      agentId: { type: 'string' },
    },
  }),
  describeTool('prepare_register_agent', 'Prepare a Solana instruction descriptor to register an agent.', {
    type: 'object',
    properties: {
      operator: { type: 'string' },
      configAccount: { type: 'string' },
      agentAccount: { type: 'string' },
      name: { type: 'string' },
      metadataHash: { type: 'string' },
      capabilityHash: { type: 'string' },
    },
    required: ['configAccount', 'operator', 'agentAccount', 'name'],
  }),
  describeTool('prepare_buy_credits', 'Prepare a COVNT credit purchase instruction descriptor.', {
    type: 'object',
    properties: {
      configAccount: { type: 'string' },
      owner: { type: 'string' },
      creditAccount: { type: 'string' },
      ownerCovntAccount: { type: 'string' },
      treasury: { type: 'string' },
      amountCovnt: { type: 'string' },
    },
    required: ['configAccount', 'owner', 'creditAccount', 'ownerCovntAccount', 'treasury', 'amountCovnt'],
  }),
  describeTool('prepare_stake', 'Prepare a COVNT stake instruction descriptor.', {
    type: 'object',
    properties: {
      configAccount: { type: 'string' },
      owner: { type: 'string' },
      agentAccount: { type: 'string' },
      positionAccount: { type: 'string' },
      ownerCovntAccount: { type: 'string' },
      stakeVault: { type: 'string' },
      amountCovnt: { type: 'string' },
      lockUntil: { type: 'string' },
    },
    required: [
      'owner',
      'configAccount',
      'agentAccount',
      'positionAccount',
      'ownerCovntAccount',
      'stakeVault',
      'amountCovnt',
      'lockUntil',
    ],
  }),
  describeTool('prepare_create_task', 'Prepare a COVNT task escrow instruction descriptor.', {
    type: 'object',
    properties: {
      configAccount: { type: 'string' },
      client: { type: 'string' },
      agentAccount: { type: 'string' },
      taskAccount: { type: 'string' },
      clientCovntAccount: { type: 'string' },
      escrowVault: { type: 'string' },
      provider: { type: 'string' },
      description: { type: 'string' },
      amountCovnt: { type: 'string' },
      deadline: { type: 'string' },
    },
    required: [
      'client',
      'configAccount',
      'agentAccount',
      'taskAccount',
      'clientCovntAccount',
      'escrowVault',
      'provider',
      'description',
      'amountCovnt',
      'deadline',
    ],
  }),
  describeTool('prepare_anchor_receipt_batch', 'Prepare a receipt Merkle-root anchoring descriptor.', {
    type: 'object',
    properties: {
      configAccount: { type: 'string' },
      authority: { type: 'string' },
      batchAccount: { type: 'string' },
      batchId: { type: 'string' },
      merkleRoot: { type: 'string' },
      receiptCount: { type: 'number' },
    },
    required: ['configAccount', 'authority', 'batchAccount', 'batchId', 'merkleRoot', 'receiptCount'],
  }),
  describeTool('derive_hash32_id', 'Derive a 32-byte hex identifier from human-readable input.', {
    type: 'object',
    properties: {
      value: { type: 'string' },
    },
    required: ['value'],
  }),
  solanaProposeTxTool,
  ...daemonTools,
];

server.setRequestHandler(ListToolsRequestSchema, async () => ({
  tools,
}));

server.setRequestHandler(CallToolRequestSchema, async (request) => {
  const args = request.params.arguments ?? {};

  try {
    switch (request.params.name) {
      case 'get_network':
        return asText({
          chain: 'solana',
          cluster: network.cluster,
          rpc_url: network.rpcUrl,
          ws_url: network.wsUrl,
          program_id: network.programId,
          covnt_mint: network.covntMint,
          explorer_url: network.explorerUrl,
        });
      case 'list_agents': {
        const { capability, operatorAddress } = listAgentsSchema.parse(args);
        return asText({
          items: MOCK_AGENT_DETAILS.filter((agent) => {
            if (capability && !agent.tags.includes(capability)) return false;
            if (operatorAddress && agent.operatorAddress !== operatorAddress) return false;
            return true;
          }),
        });
      }
      case 'list_tasks': {
        const { status, agentId } = listTasksSchema.parse(args);
        return asText({
          items: MOCK_TASKS.filter((task) => {
            if (status && task.status !== status) return false;
            if (agentId && task.agentId !== agentId) return false;
            return true;
          }),
        });
      }
      case 'prepare_register_agent': {
        const { configAccount, operator, agentAccount, name, metadataHash, capabilityHash } =
          registerAgentSchema.parse(args);
        return asText(
          prepareRegisterAgentInstruction({
            configAccount,
            operator,
            agentAccount,
            agentKey: hash32FromText(name),
            metadataHash: metadataHash ?? hash32FromText(`${name}:metadata`),
            capabilityHash: capabilityHash ?? hash32FromText(`${name}:capabilities`),
          }),
        );
      }
      case 'prepare_buy_credits':
        return asText(prepareBuyCreditsInstruction(buyCreditsSchema.parse(args)));
      case 'prepare_stake':
        return asText(prepareStakeInstruction(stakeSchema.parse(args)));
      case 'prepare_create_task': {
        const parsed = createTaskSchema.parse(args);
        const taskId = parsed.taskId ?? randomHash32();
        return asText(
          prepareCreateTaskInstruction({
            ...parsed,
            taskId,
            taskHash: hash32FromText(parsed.description),
            criteriaHash: hash32FromText(`criteria:${parsed.description}`),
          }),
        );
      }
      case 'prepare_anchor_receipt_batch':
        return asText(prepareAnchorReceiptBatchInstruction(receiptBatchSchema.parse(args)));
      case 'solana_propose_tx':
        return asText(buildSolanaProposal(args));
      case 'derive_hash32_id': {
        const { value } = deriveIdSchema.parse(args);
        return asText({
          value,
          hash32: hash32FromText(value),
        });
      }
      default:
        return (await callDaemonTool(request.params.name, args)) ?? asError(`Unknown tool: ${request.params.name}`);
    }
  } catch (error) {
    if (error instanceof z.ZodError) {
      return asError(error.issues.map((issue) => issue.message).join('; '));
    }

    if (error instanceof Error) {
      return asError(error.message);
    }

    return asError('Unknown tool execution failure');
  }
});

const isEntry = import.meta.url === `file://${process.argv[1]}`;
if (isEntry) {
  process.on('unhandledRejection', (reason) => {
    process.stderr.write(`mcp-bridge: unhandled rejection: ${reason instanceof Error ? reason.message : String(reason)}\n`);
    process.exit(1);
  });
  process.on('uncaughtException', (err) => {
    process.stderr.write(`mcp-bridge: uncaught exception: ${err.message}\n`);
    process.exit(1);
  });
  const transport = new StdioServerTransport();
  await server.connect(transport);
}
