import { McpServer } from '@modelcontextprotocol/sdk/server/mcp.js';
import { StdioServerTransport } from '@modelcontextprotocol/sdk/server/stdio.js';
import { z } from 'zod';
import { MizukiMcpClient } from './mcp-api.js';

const baseUrl = process.env.MIZUKI_API_URL ?? 'http://127.0.0.1:8787';
const apiToken = process.env.MIZUKI_API_TOKEN;
const timeoutValue = process.env.MIZUKI_MCP_TIMEOUT_MS;
const client = new MizukiMcpClient({
  baseUrl,
  apiToken,
  ...(timeoutValue ? { timeoutMs: Number(timeoutValue) } : {}),
});
const server = new McpServer({ name: 'mizuki', version: '0.1.0' });
const repositorySegment = z
  .string()
  .min(1)
  .max(100)
  .regex(/^[A-Za-z0-9_.-]+$/);

server.registerTool(
  'mizuki_quote',
  {
    description:
      'Get a fixed-price quote and x402 payment requirements for a public GitHub issue. With MIZUKI_API_TOKEN, the repository must already be connected and the quote is linked for payment recovery.',
    inputSchema: { github_issue_url: z.string().url() },
  },
  async ({ github_issue_url }) => result(await client.quote(github_issue_url)),
);

server.registerTool(
  'mizuki_submit',
  {
    description: 'Submit a quoted job with a wallet-created x402 PAYMENT-SIGNATURE.',
    inputSchema: {
      quote_id: z.string().uuid(),
      payment_signature: z.string().min(1),
      idempotency_key: z.string().min(8).max(128),
    },
  },
  async ({ quote_id, payment_signature, idempotency_key }) =>
    result(
      await client.call('/v1/jobs', {
        method: 'POST',
        body: { quote_id },
        headers: {
          'payment-signature': payment_signature,
          'idempotency-key': idempotency_key,
        },
      }),
    ),
);

server.registerTool(
  'mizuki_status',
  {
    description: 'Read delivery, PR, validation, or refund status for a Mizuki job.',
    inputSchema: { job_id: z.string().uuid() },
  },
  async ({ job_id }) => result(await client.call(`/v1/jobs/${job_id}`)),
);

server.registerTool(
  'mizuki_repositories',
  {
    description:
      'List repositories linked to the authenticated maintainer and their current readiness. Requires a repositories:read MIZUKI_API_TOKEN.',
    inputSchema: {},
  },
  async () => result(await client.repositories()),
);

server.registerTool(
  'mizuki_repository_readiness',
  {
    description:
      'Read current readiness for one repository already linked to the authenticated maintainer. Never bypasses maintainer or GitHub App checks.',
    inputSchema: { owner: repositorySegment, repo: repositorySegment },
  },
  async ({ owner, repo }) => result(await client.repositoryReadiness(owner, repo)),
);

server.registerTool(
  'mizuki_repository_issues',
  {
    description:
      'List bounded maintenance candidates for a linked repository after authenticated maintainer checks. Requires a repositories:read MIZUKI_API_TOKEN.',
    inputSchema: { owner: repositorySegment, repo: repositorySegment },
  },
  async ({ owner, repo }) => result(await client.issues(owner, repo)),
);

server.registerTool(
  'mizuki_preflight',
  {
    description:
      'Run repository, authorization, scope, and maintainer readiness checks for one issue without creating a quote or payment. Requires a repositories:read MIZUKI_API_TOKEN.',
    inputSchema: { github_issue_url: z.string().url() },
  },
  async ({ github_issue_url }) => result(await client.preflight(github_issue_url)),
);

server.registerTool(
  'mizuki_payment_status',
  {
    description:
      'Safely check whether an exact quote and idempotency key already reserved a job. This read never requests a wallet signature or submits payment. Requires a jobs:read MIZUKI_API_TOKEN.',
    inputSchema: {
      quote_id: z.string().uuid(),
      idempotency_key: z.string().min(8).max(128),
    },
  },
  async ({ quote_id, idempotency_key }) =>
    result(await client.paymentStatus(quote_id, idempotency_key)),
);

server.registerTool(
  'mizuki_bounties',
  {
    description:
      'List public maintenance bounties created after eligible jobs receive a full refund.',
    inputSchema: {},
  },
  async () => result(await client.call('/v1/bounties')),
);

server.registerTool(
  'mizuki_bounty',
  {
    description:
      'Inspect one maintenance bounty, its claim requirements, separate AI review, maintainer approval, and funded SOL escrow status.',
    inputSchema: { bounty_id: z.string().uuid() },
  },
  async ({ bounty_id }) => result(await client.call(`/v1/bounties/${bounty_id}`)),
);

server.registerTool(
  'mizuki_treasury',
  {
    description:
      'Inspect the refund reserve wallet status and planning estimates derived from service records. Planning estimates do not prove custody or grant spending authority.',
    inputSchema: {},
  },
  async () => result(await client.call('/v1/treasury')),
);

server.registerTool(
  'mizuki_capabilities',
  {
    description:
      'Inspect proposed capability changes and the evidence required before a change can reach production.',
    inputSchema: {},
  },
  async () => result(await client.call('/v1/capabilities')),
);

server.registerTool(
  'mizuki_capability_handoff',
  {
    description:
      'Read a hashed record of the failures, benchmark, and approvals required before a capability change can reach production.',
    inputSchema: { capability_id: z.string().uuid() },
  },
  async ({ capability_id }) =>
    result(await client.call(`/v1/capabilities/${encodeURIComponent(capability_id)}/handoff`)),
);

function result(value: unknown) {
  return { content: [{ type: 'text' as const, text: JSON.stringify(value, null, 2) }] };
}

await server.connect(new StdioServerTransport());
