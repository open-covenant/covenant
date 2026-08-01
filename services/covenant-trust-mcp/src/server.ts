import type {Server as HttpServer} from 'node:http';
import {pathToFileURL} from 'node:url';
import {McpServer} from '@modelcontextprotocol/sdk/server/mcp.js';
import {StreamableHTTPServerTransport} from '@modelcontextprotocol/sdk/server/streamableHttp.js';
import {StdioServerTransport} from '@modelcontextprotocol/sdk/server/stdio.js';
import {isAddress} from '@solana/kit';
import express, {type NextFunction, type Request, type Response} from 'express';
import {z} from 'zod';
import {openApiDocument} from './openapi.js';
import {getPassport} from './passport.js';
import {getPaymentHistory, type PaymentHistory} from './payment-history.js';
import {verifyAttestation, type Attestation, type VerifyResult} from './verify.js';

const VERSION = '0.2.0';
const SERVICE = 'covenant-trust';

function envInteger(name: string, fallback: number, min: number, max: number): number {
  const raw = process.env[name];
  if (raw === undefined) return fallback;
  const parsed = Number(raw);
  if (!Number.isInteger(parsed) || parsed < min || parsed > max) {
    throw new Error(`${name} must be an integer from ${min} to ${max}`);
  }
  return parsed;
}

const RPC_URL =
  process.env.COVENANT_SOLANA_MAINNET_RPC_URL ?? 'https://api.mainnet-beta.solana.com';
const RPC_TIMEOUT = envInteger('RPC_TIMEOUT_MS', 9_000, 1_000, 60_000);
const PAYMENT_HISTORY_LIMIT = envInteger('PAYMENT_HISTORY_LIMIT', 100, 1, 1_000);
const RATE_LIMIT = envInteger('RATE_LIMIT_PER_MINUTE', 30, 1, 10_000);
const PORT = envInteger('PORT', 8_930, 1, 65_535);

function isSolanaAddress(value: unknown): value is string {
  return typeof value === 'string' && isAddress(value);
}

function microUsdc(value: string): string {
  const padded = value.padStart(7, '0');
  const whole = padded.slice(0, -6);
  const fraction = padded.slice(-6).replace(/0+$/, '');
  return fraction ? `${whole}.${fraction}` : whole;
}

function paymentHistoryText(history: PaymentHistory): string {
  return (
    `Observed PayAI-sponsored USDC transfers for ${history.wallet}\n` +
    `${history.observed_inbound_transfers} inbound transfers · ` +
    `${history.distinct_senders} distinct senders · ` +
    `${microUsdc(history.volume_micro_usdc)} USDC\n` +
    `Coverage: ${history.coverage.signatures_scanned} fetched transactions from the latest ` +
    `${history.coverage.signatures_returned} PayAI fee-payer entries; ` +
    `${history.coverage.signatures_unavailable} transaction(s) unavailable. ` +
    `No x402 request, receipt, or completed job is inferred.`
  );
}

function passportText(passport: Awaited<ReturnType<typeof getPassport>>['body']): string {
  if ('error' in passport) return passport.error;
  const registration =
    passport.registry.accountOwnerMatches === null
      ? 'unknown'
      : passport.registry.accountOwnerMatches
        ? 'yes'
        : 'no';
  const validations = passport.validationRecords;
  const validationText = validations
    ? `${validations.count} record-authentic v1 record(s) observed in a non-complete ` +
      `${validations.coverage.method} scan`
    : 'v1 record lookup unavailable';
  const legacyText = passport.legacyAttestation
    ? ` · explicit legacy record ${passport.legacyAttestation.asset}`
    : '';
  return (
    `Agent passport ${passport.asset.id}\n` +
    `name ${passport.asset.name || '(unnamed)'} · owner ${passport.asset.owner}\n` +
    `registry account owner match ${registration} · ` +
    `Covenant collection ${passport.asset.inCovenantCollection ? 'yes' : 'no'}\n` +
    `${validationText}` +
    (validations?.latestObserved?.responseHash
      ? ` · latest observed ${validations.latestObserved.responseHash}`
      : '') +
    legacyText
  );
}

function parseAttestation(value: unknown): Attestation | null {
  try {
    const parsed = typeof value === 'string' ? JSON.parse(value) : value;
    return parsed && typeof parsed === 'object' ? (parsed as Attestation) : null;
  } catch {
    return null;
  }
}

function verificationText(result: VerifyResult, expectedSigner?: string): string {
  if (!result.ok) return `FAIL · ${result.reason}`;
  if (expectedSigner !== undefined && !result.signerMatches) {
    return `FAIL · signature is valid but signer ${result.signer} does not match ${expectedSigner}`;
  }
  return (
    `PASS · signature valid\nsubject ${result.subject}\nsigner ${result.signer}` +
    (expectedSigner !== undefined
      ? '\nexpected signer matched'
      : '\nauthorship not evaluated; no expected signer supplied')
  );
}

export function buildServer(): McpServer {
  const server = new McpServer(
    {name: SERVICE, title: 'Covenant Trust', version: VERSION},
    {
      instructions:
        'Covenant Trust exposes independently checkable identity, payment history, and signature ' +
        'facts. It does not issue a universal trusted/untrusted verdict; the ' +
        'calling agent applies its own policy. Every tool is read-only or pure cryptographic verification.',
    },
  );

  server.registerTool(
    'covenant_payment_history',
    {
      title: 'Observed PayAI-sponsored transfer history',
      description:
        'Coverage-limited inbound USDC transfers found in recent transactions sponsored by the PayAI ' +
        'fee payer. Reports missing transactions explicitly. Fee sponsorship alone does not prove an ' +
        'x402 request, settlement receipt, completed job, or reputation.',
      inputSchema: {wallet: z.string().describe('Solana wallet address (base58)')},
      annotations: {readOnlyHint: true, openWorldHint: true},
    },
    async ({wallet}) => {
      if (!isSolanaAddress(wallet)) {
        return {content: [{type: 'text', text: 'not a valid Solana wallet address'}], isError: true};
      }
      try {
        const history = await getPaymentHistory(
          RPC_URL,
          RPC_TIMEOUT,
          wallet,
          PAYMENT_HISTORY_LIMIT,
        );
        return {
          content: [{type: 'text', text: paymentHistoryText(history)}],
          structuredContent: history as unknown as Record<string, unknown>,
        };
      } catch {
        return {
          content: [{type: 'text', text: 'Solana RPC unavailable'}],
          isError: true,
        };
      }
    },
  );

  server.registerTool(
    'covenant_agent_passport',
    {
      title: 'Agent identity and validation records',
      description:
        "Partial identity facts for an MPL Core agent asset plus Covenant validation envelopes whose " +
        'AppData write authority and payload fields pass record-authenticity checks. This endpoint does ' +
        'not fully decode the MIP-014 registry account or verify committed evidence. On-chain URIs are ' +
        'returned as untrusted data and are never fetched.',
      inputSchema: {asset: z.string().describe('MPL Core asset address (base58)')},
      annotations: {readOnlyHint: true, openWorldHint: true},
    },
    async ({asset}) => {
      const result = await getPassport(RPC_URL, RPC_TIMEOUT, asset);
      if (result.status !== 200 || 'error' in result.body) {
        const message = 'error' in result.body ? result.body.error : 'agent lookup failed';
        return {content: [{type: 'text', text: message}], isError: true};
      }
      return {
        content: [{type: 'text', text: passportText(result.body)}],
        structuredContent: result.body as unknown as Record<string, unknown>,
      };
    },
  );

  server.registerTool(
    'covenant_verify',
    {
      title: 'Verify attestation signature',
      description:
        'Verify the integrity of a covenant.attest.v1 envelope. A valid signature proves authorship by ' +
        'the carried signer, not that the signer is trusted. Supply expected_signer from an independent ' +
        'trust source when authorship matters.',
      inputSchema: {
        attestation: z
          .union([z.string(), z.record(z.unknown())])
          .describe('Attestation JSON object or serialized JSON'),
        expected_signer: z
          .string()
          .optional()
          .describe('Expected base58 Ed25519 public key from an independent trust source'),
      },
      annotations: {readOnlyHint: true, openWorldHint: false, idempotentHint: true},
    },
    async ({attestation, expected_signer}) => {
      const parsed = parseAttestation(attestation);
      if (!parsed) {
        return {content: [{type: 'text', text: 'attestation is not valid JSON'}], isError: true};
      }
      if (expected_signer !== undefined && !isSolanaAddress(expected_signer)) {
        return {content: [{type: 'text', text: 'expected_signer is not a valid Ed25519 public key'}], isError: true};
      }
      const result = verifyAttestation(parsed, expected_signer);
      return {
        content: [{type: 'text', text: verificationText(result, expected_signer)}],
        isError: !result.ok || result.signerMatches === false,
        structuredContent: result as unknown as Record<string, unknown>,
      };
    },
  );

  return server;
}

type RateEntry = {count: number; resetAt: number};

function rateLimiter(limit: number) {
  const entries = new Map<string, RateEntry>();
  return (req: Request, res: Response, next: NextFunction): void => {
    const now = Date.now();
    const key = req.ip || 'unknown';
    let entry = entries.get(key);
    if (!entry || entry.resetAt <= now) {
      entry = {count: 0, resetAt: now + 60_000};
      entries.set(key, entry);
    }
    entry.count += 1;
    res.setHeader('RateLimit-Limit', String(limit));
    res.setHeader('RateLimit-Remaining', String(Math.max(0, limit - entry.count)));
    res.setHeader('RateLimit-Reset', String(Math.ceil(entry.resetAt / 1_000)));
    if (entry.count > limit) {
      res.status(429).json({error: 'rate limit exceeded'});
      return;
    }
    if (entries.size > 10_000) {
      for (const [candidate, value] of entries) {
        if (value.resetAt <= now) entries.delete(candidate);
      }
    }
    next();
  };
}

export function createApp(): express.Express {
  const app = express();
  app.disable('x-powered-by');
  app.set('trust proxy', 1);
  app.use(express.json({limit: '1mb'}));

  app.get('/health', (_req, res) => {
    res.json({ok: true, service: SERVICE, version: VERSION});
  });
  app.get('/', (_req, res) => {
    res.json({
      service: SERVICE,
      version: VERSION,
      purpose: 'catalog-neutral, pre-payment trust facts for agents',
      policy: 'facts only; the caller decides whether to allow, review, or deny',
      mcp: '/mcp',
      openapi: '/openapi.json',
    });
  });
  app.get('/openapi.json', (_req, res) => res.json(openApiDocument));

  const limited = rateLimiter(RATE_LIMIT);
  app.use('/mcp', limited);
  app.use('/v1', limited);
  app.use('/v1', (_req, res, next) => {
    res.setHeader('Cache-Control', 'no-store');
    next();
  });

  app.get('/v1/payment-history/:wallet', async (req, res) => {
    if (!isSolanaAddress(req.params.wallet)) {
      res.status(400).json({error: 'not a valid Solana wallet address'});
      return;
    }
    try {
      res.json(
        await getPaymentHistory(
          RPC_URL,
          RPC_TIMEOUT,
          req.params.wallet,
          PAYMENT_HISTORY_LIMIT,
        ),
      );
    } catch {
      res.status(503).json({error: 'Solana RPC unavailable'});
    }
  });

  app.get('/v1/agents/:asset', async (req, res) => {
    const result = await getPassport(RPC_URL, RPC_TIMEOUT, req.params.asset);
    res.status(result.status).json(result.body);
  });

  app.post('/v1/attestations/verify', (req, res) => {
    const attestation = parseAttestation(req.body?.attestation);
    const expectedSigner = req.body?.expected_signer;
    if (!attestation) {
      res.status(400).json({error: 'attestation must be a JSON object or serialized JSON'});
      return;
    }
    if (expectedSigner !== undefined && !isSolanaAddress(expectedSigner)) {
      res.status(400).json({error: 'expected_signer is not a valid Ed25519 public key'});
      return;
    }
    res.json(verifyAttestation(attestation, expectedSigner));
  });

  app.post('/mcp', async (req, res) => {
    const server = buildServer();
    const transport = new StreamableHTTPServerTransport({sessionIdGenerator: undefined});
    res.on('close', () => {
      void transport.close();
      void server.close();
    });
    try {
      await server.connect(transport);
      await transport.handleRequest(req, res, req.body);
    } catch {
      if (!res.headersSent) {
        res.status(500).json({
          jsonrpc: '2.0',
          error: {code: -32603, message: 'internal error'},
          id: null,
        });
      }
    }
  });

  const noGet = (_req: Request, res: Response) => {
    res.status(405).json({
      jsonrpc: '2.0',
      error: {code: -32000, message: 'Method not allowed. POST to /mcp.'},
      id: null,
    });
  };
  app.get('/mcp', noGet);
  app.delete('/mcp', noGet);

  return app;
}

export async function serveHttp(): Promise<HttpServer> {
  const app = createApp();
  return new Promise((resolve, reject) => {
    const server = app.listen(PORT, '0.0.0.0', () => {
      console.error(`${SERVICE} listening on :${PORT}`);
      resolve(server);
    });
    server.on('error', reject);
  });
}

export async function serveStdio(): Promise<void> {
  const transport = new StdioServerTransport();
  await buildServer().connect(transport);
}

const entrypoint =
  process.argv[1] !== undefined && pathToFileURL(process.argv[1]).href === import.meta.url;

if (entrypoint) {
  const run = process.argv.includes('--stdio') ? serveStdio : serveHttp;
  run().catch((error) => {
    console.error(error);
    process.exit(1);
  });
}
