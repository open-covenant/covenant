import type { Tool } from '@modelcontextprotocol/sdk/types.js';
import { z } from 'zod';
import { isSolanaAddress } from '@covenant/sdk';

// Mirror of the daemon's native `solana_propose_tx` tool: structure and
// validate an unsigned Solana transaction proposal in the `@covenant/sdk`
// bundle shape. It never signs, never sends, and holds no keypair — simulation,
// capability-gating, and signing are the daemon broker's job downstream.

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

// Resolve the cluster the way Covenant does elsewhere: an explicit per-call
// `cluster` wins, otherwise the `COVENANT_SOLANA_CLUSTER` env (the pin the
// auto-install config sets), otherwise devnet. This intentionally covers only
// the cluster key — not the SDK's per-cluster RPC-URL env overrides — since a
// proposal needs just a label and a default RPC. An unrecognised value falls
// back to devnet; `mainnet`/`mainnet-beta` resolve to the `mainnet-beta` label.
// The tool only structures the proposal; the daemon still refuses to sign
// outside devnet.
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

/** Build an unsigned proposal bundle from tool arguments. Throws `ZodError` on
 * malformed input; the caller surfaces that as a tool error. */
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
