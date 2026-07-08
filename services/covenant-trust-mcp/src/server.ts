// Covenant Trust: a zero-install MCP for agents. Add it to Claude Code or Codex
// and, with no prior setup, the agent can check any Solana wallet's on-chain
// reputation, read any agent's identity passport, and verify a Covenant-signed
// attestation. Every tool is a pure read or pure crypto: no keys, no local
// state, no payment, nothing to install.
//
//   HTTP (remote, zero-install):  node dist/server.js         -> POST /mcp
//   stdio (local / npx):          node dist/server.js --stdio

import {McpServer} from '@modelcontextprotocol/sdk/server/mcp.js';
import {StreamableHTTPServerTransport} from '@modelcontextprotocol/sdk/server/streamableHttp.js';
import {StdioServerTransport} from '@modelcontextprotocol/sdk/server/stdio.js';
import {z} from 'zod';
import express from 'express';
import {getReputation, type Reputation} from './reputation.js';
import {getPassport} from './passport.js';
import {verifyAttestation, type Attestation} from './verify.js';

const RPC_URL = process.env.COVENANT_SOLANA_MAINNET_RPC_URL ?? 'https://api.mainnet-beta.solana.com';
const RPC_TIMEOUT = Number(process.env.RPC_TIMEOUT_MS ?? 9000);
const REPUTATION_LIMIT = Number(process.env.REPUTATION_LIMIT ?? 100);
const PORT = Number(process.env.PORT ?? 8930);

const SOLANA_ADDRESS = /^[1-9A-HJ-NP-Za-km-z]{32,44}$/;

function reputationText(r: Reputation): string {
  return (
    `Covenant reputation for ${r.wallet}\n` +
    `score ${r.score}/1000 · ${r.tier}\n` +
    `${r.settled_jobs} settled jobs · ${r.distinct_counterparties} distinct counterparties · ` +
    `$${(r.volume_micro_usdc / 1_000_000).toLocaleString('en-US')} USDC inbound\n` +
    `Grounded in public on-chain USDC settlements. Self-payments excluded.`
  );
}

function buildServer(): McpServer {
  const server = new McpServer(
    {name: 'covenant-guard', title: 'Covenant Guard', version: '0.1.0'},
    {
      instructions:
        'Covenant Guard exposes on-chain trust facts for agents. Use covenant_reputation before ' +
        'transacting with or trusting a Solana wallet, covenant_agent_passport to check an agent asset\'s ' +
        'registered identity and attestation, and covenant_verify to check a Covenant-signed receipt or ' +
        'attestation. All tools are read-only and take no credentials.',
    },
  );

  server.registerTool(
    'covenant_reputation',
    {
      title: 'Wallet reputation',
      description:
        "A Solana wallet's reputation (0-1000) grounded in public on-chain USDC settlements: how many " +
        'jobs it settled, from how many distinct counterparties, and total inbound volume. Use before ' +
        'trusting or paying a counterparty. Self-payments are excluded so a wallet cannot inflate itself.',
      inputSchema: {wallet: z.string().describe('Solana wallet address (base58)')},
      annotations: {readOnlyHint: true, openWorldHint: true},
    },
    async ({wallet}) => {
      if (!SOLANA_ADDRESS.test(wallet)) {
        return {content: [{type: 'text', text: 'not a valid Solana wallet address'}], isError: true};
      }
      const r = await getReputation(RPC_URL, RPC_TIMEOUT, wallet, REPUTATION_LIMIT);
      return {content: [{type: 'text', text: reputationText(r)}], structuredContent: r as unknown as Record<string, unknown>};
    },
  );

  server.registerTool(
    'covenant_agent_passport',
    {
      title: 'Agent passport',
      description:
        "An MPL Core agent asset's on-chain identity: whether it is registered in the Agent Identity " +
        'registry, and whether it carries a Covenant attestation and who authored it. Use to check that ' +
        'an agent is who it claims to be before trusting it.',
      inputSchema: {asset: z.string().describe('MPL Core asset address (base58)')},
      annotations: {readOnlyHint: true, openWorldHint: true},
    },
    async ({asset}) => {
      const res = await getPassport(RPC_URL, RPC_TIMEOUT, asset);
      if (res.status !== 200 && 'error' in res.body) {
        return {content: [{type: 'text', text: res.body.error}], isError: true};
      }
      const p = res.body as Extract<typeof res.body, {asset: unknown}>;
      const text =
        `Agent passport ${p.asset.id}\n` +
        `name ${p.asset.name || '(unnamed)'} · owner ${p.asset.owner}\n` +
        `registered ${p.registry.registered ? 'yes' : 'no'} · ` +
        `covenant collection ${p.asset.inCovenantCollection ? 'yes' : 'no'}\n` +
        `attestation ${p.attestation ? (p.attestation.covenantAuthored ? 'Covenant-authored' : 'present') : 'none'}`;
      return {content: [{type: 'text', text}], structuredContent: p as unknown as Record<string, unknown>};
    },
  );

  server.registerTool(
    'covenant_verify',
    {
      title: 'Verify attestation',
      description:
        'Verify a Covenant-signed attestation (ed25519 over a domain-separated SHA-256 of the ' +
        'canonical payload). Returns whether the signature matches the contents, so tampering with any ' +
        'field fails. Pass the attestation JSON object.',
      inputSchema: {attestation: z.union([z.string(), z.record(z.unknown())]).describe('Attestation JSON (object or string)')},
      annotations: {readOnlyHint: true, openWorldHint: false, idempotentHint: true},
    },
    async ({attestation}) => {
      let att: Attestation;
      try {
        att = (typeof attestation === 'string' ? JSON.parse(attestation) : attestation) as Attestation;
      } catch {
        return {content: [{type: 'text', text: 'attestation is not valid JSON'}], isError: true};
      }
      const v = verifyAttestation(att);
      const text = v.ok
        ? `PASS · signature valid\nsubject ${v.subject}\nsigner ${v.signer}`
        : `FAIL · ${v.reason}`;
      return {content: [{type: 'text', text}], isError: !v.ok, structuredContent: v as unknown as Record<string, unknown>};
    },
  );

  return server;
}

async function serveHttp(): Promise<void> {
  const app = express();
  app.use(express.json({limit: '1mb'}));
  app.get('/health', (_req, res) => res.json({ok: true, service: 'covenant-trust-mcp'}));
  // Stateless: a fresh server + transport per request, so there is no session to
  // manage and the endpoint scales horizontally.
  app.post('/mcp', async (req, res) => {
    const server = buildServer();
    const transport = new StreamableHTTPServerTransport({sessionIdGenerator: undefined});
    res.on('close', () => {
      transport.close();
      server.close();
    });
    try {
      await server.connect(transport);
      await transport.handleRequest(req, res, req.body);
    } catch (e) {
      if (!res.headersSent) {
        res.status(500).json({jsonrpc: '2.0', error: {code: -32603, message: e instanceof Error ? e.message : 'internal error'}, id: null});
      }
    }
  });
  const noGet = (_req: express.Request, res: express.Response) =>
    res.status(405).json({jsonrpc: '2.0', error: {code: -32000, message: 'Method not allowed. POST to /mcp.'}, id: null});
  app.get('/mcp', noGet);
  app.delete('/mcp', noGet);
  app.listen(PORT, () => console.error(`covenant-trust-mcp on :${PORT} (POST /mcp)`));
}

async function serveStdio(): Promise<void> {
  const transport = new StdioServerTransport();
  await buildServer().connect(transport);
}

if (process.argv.includes('--stdio')) {
  serveStdio().catch((e) => {
    console.error(e);
    process.exit(1);
  });
} else {
  serveHttp().catch((e) => {
    console.error(e);
    process.exit(1);
  });
}
