import { McpServer } from '@modelcontextprotocol/sdk/server/mcp.js';
import { StdioServerTransport } from '@modelcontextprotocol/sdk/server/stdio.js';
import { z } from 'zod';

const baseUrl = process.env.MIZUKI_API_URL ?? 'http://127.0.0.1:8787';
const server = new McpServer({ name: 'mizuki', version: '0.1.0' });

server.registerTool(
  'mizuki_quote',
  {
    description: 'Get a fixed-price quote and x402 payment requirements for a public GitHub issue.',
    inputSchema: { github_issue_url: z.string().url() },
  },
  async ({ github_issue_url }) => result(await call('/v1/quotes', { github_issue_url })),
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
      await call(
        '/v1/jobs',
        { quote_id },
        {
          'payment-signature': payment_signature,
          'idempotency-key': idempotency_key,
        },
      ),
    ),
);

server.registerTool(
  'mizuki_status',
  {
    description: 'Read delivery, PR, validation, or refund status for a Mizuki job.',
    inputSchema: { job_id: z.string().uuid() },
  },
  async ({ job_id }) => result(await call(`/v1/jobs/${job_id}`)),
);

server.registerTool(
  'mizuki_bounties',
  {
    description: 'List public rescue bounties created from fully refunded maintenance jobs.',
    inputSchema: {},
  },
  async () => result(await call('/v1/bounties')),
);

server.registerTool(
  'mizuki_bounty',
  {
    description: 'Inspect one rescue bounty, its claim, review, and contributor escrow state.',
    inputSchema: { bounty_id: z.string().uuid() },
  },
  async ({ bounty_id }) => result(await call(`/v1/bounties/${bounty_id}`)),
);

server.registerTool(
  'mizuki_treasury',
  {
    description:
      'Inspect signer-verified refund custody and the separate application-ledger allocation model.',
    inputSchema: {},
  },
  async () => result(await call('/v1/treasury')),
);

server.registerTool(
  'mizuki_capabilities',
  {
    description: 'Inspect Mizuki capability proposals and externally authorized upgrade evidence.',
    inputSchema: {},
  },
  async () => result(await call('/v1/capabilities')),
);

server.registerTool(
  'mizuki_capability_handoff',
  {
    description:
      'Read the hashed failure and benchmark handoff for an independent upgrade authority.',
    inputSchema: { capability_id: z.string().uuid() },
  },
  async ({ capability_id }) =>
    result(await call(`/v1/capabilities/${encodeURIComponent(capability_id)}/handoff`)),
);

async function call(path: string, body?: unknown, headers: Record<string, string> = {}) {
  const response = await fetch(`${baseUrl.replace(/\/$/, '')}${path}`, {
    method: body === undefined ? 'GET' : 'POST',
    headers: { ...(body === undefined ? {} : { 'content-type': 'application/json' }), ...headers },
    body: body === undefined ? undefined : JSON.stringify(body),
  });
  const value = await response.json();
  if (!response.ok) throw new Error(`Mizuki API ${response.status}: ${JSON.stringify(value)}`);
  return value;
}

function result(value: unknown) {
  return { content: [{ type: 'text' as const, text: JSON.stringify(value, null, 2) }] };
}

await server.connect(new StdioServerTransport());
