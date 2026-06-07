import type { Tool } from '@modelcontextprotocol/sdk/types.js';
import { z } from 'zod';
import { isSolanaAddress } from '@covenant/sdk';

// Pure builder for an unsigned `@covenant/sdk` proposal bundle. Holds no keypair
// and never signs or sends — simulation, capability-gating, and signing happen
// in the daemon broker downstream.

const addressSchema = z.string().refine(isSolanaAddress, 'expected a Solana address');

const accountMetaSchema = z
  .object({
    name: z.string().min(1),
    address: addressSchema,
    signer: z.boolean(),
    writable: z.boolean(),
  })
  .strict();

const dataValueSchema = z.union([z.string(), z.number(), z.boolean(), z.null()]);

export const proposeTxSchema = z
  .object({
    programId: addressSchema,
    instruction: z.string().min(1),
    accounts: z.array(accountMetaSchema).min(1),
    data: z.record(z.string(), dataValueSchema),
    cluster: z.string().optional(),
    rpcUrl: z.string().min(1).optional(),
  })
  .strict();

type ClusterNetwork = { cluster: string; rpcUrl: string };

// Explicit `cluster` wins, then the COVENANT_SOLANA_CLUSTER pin, else devnet.
// Per-cluster RPC-URL env overrides are deliberately ignored — a proposal needs
// only a label and a default RPC.
function clusterNetwork(cluster: string | undefined, env: NodeJS.ProcessEnv): ClusterNetwork {
  switch (cluster ?? env.COVENANT_SOLANA_CLUSTER) {
    case 'localnet':
      return { cluster: 'localnet', rpcUrl: 'http://127.0.0.1:8899' };
    case 'mainnet':
    case 'mainnet-beta':
      return { cluster: 'mainnet-beta', rpcUrl: 'https://api.mainnet-beta.solana.com' };
    default:
      return { cluster: 'devnet', rpcUrl: 'https://api.devnet.solana.com' };
  }
}

export type SolanaProposal = {
  chain: 'solana';
  cluster: string;
  rpcUrl: string;
  instructions: Array<{
    programId: string;
    instruction: string;
    accounts: Array<{ name: string; address: string; signer: boolean; writable: boolean }>;
    data: Record<string, string | number | boolean | null>;
  }>;
};

/** Throws `ZodError` on malformed input; the caller surfaces it as a tool error. */
export function buildSolanaProposal(args: unknown, env: NodeJS.ProcessEnv = process.env): SolanaProposal {
  const parsed = proposeTxSchema.parse(args);
  const network = clusterNetwork(parsed.cluster, env);
  return {
    chain: 'solana',
    cluster: network.cluster,
    rpcUrl: parsed.rpcUrl ?? network.rpcUrl,
    instructions: [
      {
        programId: parsed.programId,
        instruction: parsed.instruction,
        accounts: parsed.accounts,
        data: parsed.data,
      },
    ],
  };
}

export const solanaProposeTxTool: Tool = {
  name: 'solana_propose_tx',
  description:
    'Build an unsigned Solana transaction proposal (programId, instruction, accounts, data) in the @covenant/sdk bundle shape. Defaults to devnet. Never signs or sends — the daemon broker simulates, capability-checks, and signs downstream.',
  inputSchema: {
    type: 'object',
    properties: {
      programId: { type: 'string' },
      instruction: { type: 'string' },
      accounts: {
        type: 'array',
        minItems: 1,
        items: {
          type: 'object',
          properties: {
            name: { type: 'string' },
            address: { type: 'string' },
            signer: { type: 'boolean' },
            writable: { type: 'boolean' },
          },
          required: ['name', 'address', 'signer', 'writable'],
          additionalProperties: false,
        },
      },
      data: { type: 'object', additionalProperties: { type: ['string', 'number', 'boolean', 'null'] } },
      cluster: { type: 'string' },
      rpcUrl: { type: 'string' },
    },
    required: ['programId', 'instruction', 'accounts', 'data'],
    additionalProperties: false,
  },
};
