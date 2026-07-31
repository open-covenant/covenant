/**
 * Covenant evidence — public x402 v2 seller.
 *
 * Agents pay per call in USDC on Solana for bounded observations and
 * publisher-signed statements. The legacy passport and reputation paths are
 * retained for compatibility; their responses are not identity or reputation.
 *
 * `@x402/express` issues the 402 challenge via a locally-registered (signer-less)
 * SVM scheme, then verifies + settles through the PayAI facilitator (which
 * sponsors the Solana fee payer). Resource delivery and settlement are separate
 * protocol outcomes. Clients must reconcile settlement state before retrying a
 * request after any facilitator, handler, or transport error.
 *
 * Env (Render vars):
 *   PORT                            listen port (Render injects)
 *   COVENANT_TREASURY               payTo — where USDC revenue lands
 *   X402_FACILITATOR_URL            facilitator base (verify/settle + feePayer)
 *   FACILITATOR_PUBKEY              sponsor feePayer advertised in the challenge
 *   X402_SYNC_FACILITATOR           "false" to skip facilitator sync at boot
 *   ZAUTH_API_KEY                   zauth provider key (telemetry; optional)
 *   COVENANT_SOLANA_MAINNET_RPC_URL DAS-capable RPC for the passport lookup
 *   COVENANT_ATTEST_KEYPAIR         64-byte JSON array — the attestation signer
 */
import { readFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import express, { type Request, type Response } from 'express';
import { paymentMiddlewareFromConfig } from '@x402/express';
import { HTTPFacilitatorClient, type RoutesConfig } from '@x402/core/server';
import { ExactSvmScheme } from '@x402/svm/exact/server';
import { declareDiscoveryExtension } from '@x402/extensions/bazaar';
import { zauthProvider } from '@zauthx402/sdk/middleware';
import { getPassport } from './passport.js';
import {
  Attestor,
  ATTEST_DOMAIN,
  ATTEST_CANONICALIZATION,
  ATTEST_VERIFY_RECIPE,
} from './attest.js';
import { getTransferActivity } from './reputation.js';
import { verifyErEnclave, ErEnclaveError } from './er-enclave.js';

const PORT = Number(process.env.PORT ?? 10000);
const PAY_TO = process.env.COVENANT_TREASURY ?? '8xbXHAhiVe2BrYDq4qpTA5SSYJG9XNjNN6jcrudhTKCM';
const FACILITATOR_URL = process.env.X402_FACILITATOR_URL ?? 'https://facilitator.payai.network';
const FEE_PAYER = process.env.FACILITATOR_PUBKEY ?? '2wKupLR9q6wXYppw8Gr2NvWxKBUqm4PPJKkQfoxHDBg4';
const SYNC = process.env.X402_SYNC_FACILITATOR !== 'false';
const ZAUTH_API_KEY = process.env.ZAUTH_API_KEY;
const RPC_URL =
  process.env.COVENANT_SOLANA_MAINNET_RPC_URL ?? 'https://api.mainnet-beta.solana.com';
const RPC_TIMEOUT = Number(process.env.RPC_TIMEOUT_MS ?? 9000);
const REPUTATION_LIMIT = Number(process.env.REPUTATION_LIMIT ?? 100);

const SOLANA_MAINNET = 'solana:5eykt4UsFv8P8NJdTREpY1vzqKqZKvdp' as const;
const USDC_SOLANA = 'EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v';

const attestor = process.env.COVENANT_ATTEST_KEYPAIR
  ? new Attestor(JSON.parse(process.env.COVENANT_ATTEST_KEYPAIR) as number[])
  : null;

const openapi = JSON.parse(
  readFileSync(join(dirname(fileURLToPath(import.meta.url)), '..', 'openapi.json'), 'utf8'),
);

const app = express();
// Render terminates TLS and forwards over http; trust the proxy so req.protocol
// reflects the real https scheme in discovery URLs.
app.set('trust proxy', true);
app.use(express.json());

app.get('/health', (_req: Request, res: Response) => {
  res.json({
    ok: true,
    service: 'covenant-x402-seller',
    resources: [
      '/x402/passport/:asset',
      '/x402/attest',
      '/x402/payai/reputation/:wallet',
      '/x402/er/enclave/:validator',
    ],
  });
});

app.get('/openapi.json', (_req: Request, res: Response) => {
  res.set('cache-control', 'public, max-age=300').json(openapi);
});

// x402 discovery — lets crawlers (zauth directory, x402scan) list the resources.
app.get('/.well-known/x402', (req: Request, res: Response) => {
  const base = `${req.protocol}://${req.get('host')}`;
  res.json({
    version: 1,
    resources: [
      `${base}/x402/passport/{asset}`,
      `${base}/x402/attest`,
      `${base}/x402/payai/reputation/{wallet}`,
      `${base}/x402/er/enclave/{validator}`,
    ],
    instructions:
      'Covenant evidence x402 seller. Paid endpoints return configured-provider structural observations for an MPL Core asset and 014 Registry binding, a publisher-signed statement over caller data, a bounded fee-payer-associated USDC transfer-activity heuristic, or a publisher-signed DCAP monitor result. Registration, transfer history, and signatures do not prove identity, delivery, quality, reputation, or W009/W011 enforcement. Resource delivery and settlement are separate outcomes; reconcile settlement before retrying.',
    // Pin this key through a trusted channel. This mutable endpoint cannot pin itself.
    attestation: attestor
      ? {
          algorithm: 'ed25519',
          publicKey: attestor.pubkeyB58,
          domain: ATTEST_DOMAIN,
          canonicalization: ATTEST_CANONICALIZATION,
          verify: ATTEST_VERIFY_RECIPE,
        }
      : null,
  });
});

if (ZAUTH_API_KEY) {
  app.use(zauthProvider(ZAUTH_API_KEY));
} else {
  console.warn('ZAUTH_API_KEY unset — running without zauth provider telemetry');
}

const facilitator = new HTTPFacilitatorClient({ url: FACILITATOR_URL });

// `extensions` carries the bazaar discovery declaration so each resource is
// listed in the facilitator's discovery catalog (x402scan, the PayAI bazaar).
// Without it the routes still settle but stay invisible to discovery crawlers.
const gate = (amount: string, description: string, extensions: Record<string, unknown>) => ({
  accepts: {
    scheme: 'exact' as const,
    network: SOLANA_MAINNET,
    payTo: PAY_TO,
    price: { asset: USDC_SOLANA, amount },
    maxTimeoutSeconds: 300,
    extra: { feePayer: FEE_PAYER },
  },
  description,
  mimeType: 'application/json',
  serviceName: 'Covenant',
  tags: ['covenant', 'evidence', 'solana', 'x402', 'agent'],
  extensions,
});

const routes: RoutesConfig = {
  'GET /x402/passport/:asset': gate(
    '1000',
    'Observe an MPL Core asset, its 014 Registry binding, and DAS-reported AppData. This does not prove identity or claim truth.',
    declareDiscoveryExtension({
      pathParams: { asset: '9sFJ95mZsBTGqTEBkcbmsx2V8RQiZ5iQACCLPLE61aWH' },
      pathParamsSchema: {
        properties: { asset: { type: 'string', description: 'MPL Core asset address (base58)' } },
        required: ['asset'],
      },
      output: {
        example: {
          asset: { id: '9sFJ95mZsBTGqTEBkcbmsx2V8RQiZ5iQACCLPLE61aWH', inCovenantCollection: true },
          registry: { registered: true },
          attestation: { matchesConfiguredAuthority: true, evidenceSource: 'configured_das' },
        },
      },
    }),
  ),
  'POST /x402/attest': gate(
    '5000',
    'Create a key-signed statement over caller data. With an externally pinned expected key, the signature verifies exact bytes; it does not independently identify the publisher or make the claim true.',
    declareDiscoveryExtension({
      input: {
        subject: '9sFJ95mZsBTGqTEBkcbmsx2V8RQiZ5iQACCLPLE61aWH',
        claim: { delivered: true },
      },
      inputSchema: {
        properties: { subject: { type: 'string' }, claim: {} },
        required: ['subject', 'claim'],
      },
      bodyType: 'json',
      output: { example: { alg: 'ed25519', signature_b58: '…', pubkey_b58: '…' } },
    }),
  ),
  'GET /x402/payai/reputation/:wallet': gate(
    '3000',
    'Summarize bounded USDC transfer activity from recent transactions associated with a configured fee-payer account. Not reputation or proof of jobs.',
    declareDiscoveryExtension({
      pathParams: { wallet: 'CvX23FNQsNQww8ALR3EWgQv2Wt5yM7VvU4HRigGBMMJu' },
      pathParamsSchema: {
        properties: {
          wallet: { type: 'string', description: 'Solana wallet (owner) address (base58)' },
        },
        required: ['wallet'],
      },
      output: {
        example: {
          activity: {
            schema: 'covenant.payai-transfer-activity.v1',
            wallet: 'CvX23FNQsNQww8ALR3EWgQv2Wt5yM7VvU4HRigGBMMJu',
            observed_inbound_transfers: 12,
            distinct_observed_senders: 8,
            observed_volume_micro_usdc: '117000',
          },
          attestation: { alg: 'ed25519', signature_b58: '…', pubkey_b58: '…' },
        },
      },
    }),
  ),
  'GET /x402/er/enclave/:validator': gate(
    '10000',
    "Run the service's DCAP verification path for a MagicBlock validator quote and sign the reported result. Consumers still trust this implementation, its issuer, and endpoint selection.",
    declareDiscoveryExtension({
      pathParams: { validator: 'MTEWGuqxUpYZGFJQcp8tLN7x5v9BSeoFHYWQQ3n3xzo' },
      pathParamsSchema: {
        properties: {
          validator: {
            type: 'string',
            description: 'ER validator identity from the Magic Router (base58)',
          },
        },
        required: ['validator'],
      },
      output: {
        example: {
          enclave: {
            validator: 'MTEWGuqxUpYZGFJQcp8tLN7x5v9BSeoFHYWQQ3n3xzo',
            tee: 'intel-tdx',
            status: 'UpToDate',
            mr_td: '…',
          },
          attestation: { alg: 'ed25519', signature_b58: '…', pubkey_b58: '…' },
        },
      },
    }),
  ),
};

app.use(
  paymentMiddlewareFromConfig(
    routes,
    facilitator,
    [{ network: SOLANA_MAINNET, server: new ExactSvmScheme() }],
    undefined,
    undefined,
    SYNC,
  ),
);

// Payment verification, handler execution, resource delivery, and settlement
// are distinct stages. Error responses make no claim about settlement state.
app.get('/x402/passport/:asset', async (req: Request, res: Response) => {
  try {
    const { status, body } = await getPassport(RPC_URL, RPC_TIMEOUT, req.params.asset);
    res.status(status).json(body);
  } catch {
    res.status(502).json({ error: 'chain/DAS upstream unavailable' });
  }
});

app.post('/x402/attest', (req: Request, res: Response) => {
  if (!attestor) {
    res.status(503).json({ error: 'attestation signer not configured' });
    return;
  }
  const { subject, claim } = (req.body ?? {}) as { subject?: unknown; claim?: unknown };
  if (typeof subject !== 'string' || !subject || subject.length > 256 || claim === undefined) {
    res.status(400).json({ error: 'subject (1–256 char string) and claim are required' });
    return;
  }
  res.json(attestor.attest(subject, claim, Math.floor(Date.now() / 1000)));
});

// Legacy route name retained for compatibility. The response is a bounded
// transfer-activity observation, not reputation.
app.get('/x402/payai/reputation/:wallet', async (req: Request, res: Response) => {
  if (!attestor) {
    res.status(503).json({ error: 'attestation signer not configured' });
    return;
  }
  const wallet = req.params.wallet;
  if (!/^[1-9A-HJ-NP-Za-km-z]{32,44}$/.test(wallet)) {
    res.status(400).json({ error: 'wallet must be a base58 Solana address' });
    return;
  }
  try {
    const activity = await getTransferActivity(RPC_URL, RPC_TIMEOUT, wallet, REPUTATION_LIMIT);
    const attestation = attestor.attest(wallet, activity, Math.floor(Date.now() / 1000));
    res.json({ activity, attestation });
  } catch {
    res.status(502).json({ error: 'chain/RPC upstream unavailable' });
  }
});

// Runs this service's DCAP verification path and signs its result. An optional
// subject only changes the challenge bytes; it does not prove record origin.
app.get('/x402/er/enclave/:validator', async (req: Request, res: Response) => {
  if (!attestor) {
    res.status(503).json({ error: 'attestation signer not configured' });
    return;
  }
  const validator = req.params.validator;
  if (!/^[1-9A-HJ-NP-Za-km-z]{32,44}$/.test(validator)) {
    res.status(400).json({ error: 'validator must be a base58 identity from the Magic Router' });
    return;
  }
  const agent = typeof req.query.agent === 'string' ? req.query.agent : undefined;
  const provenanceRoot =
    typeof req.query.provenance_root === 'string' ? req.query.provenance_root : undefined;
  if ((agent === undefined) !== (provenanceRoot === undefined)) {
    res
      .status(400)
      .json({ error: 'agent and provenance_root bind together — pass both or neither' });
    return;
  }
  if (agent && !/^[1-9A-HJ-NP-Za-km-z]{32,44}$/.test(agent)) {
    res.status(400).json({ error: 'agent must be a base58 Solana address' });
    return;
  }
  if (provenanceRoot && !/^[0-9a-fA-F]{64}$/.test(provenanceRoot)) {
    res.status(400).json({ error: 'provenance_root must be 32-byte hex' });
    return;
  }
  try {
    const enclave = await verifyErEnclave(validator, { agent, provenanceRoot });
    const attestation = attestor.attest(validator, enclave, Math.floor(Date.now() / 1000));
    res.json({ enclave, attestation });
  } catch (e) {
    if (e instanceof ErEnclaveError) {
      res.status(e.status).json({ error: e.message });
      return;
    }
    console.error('er enclave verify failed:', e);
    res.status(502).json({ error: 'enclave verification upstream unavailable' });
  }
});

app.listen(PORT, () => {
  console.log(
    `covenant-x402-seller on :${PORT} — paid /x402/passport/:asset + /x402/attest + /x402/payai/reputation/:wallet + /x402/er/enclave/:validator, payTo ${PAY_TO}, facilitator ${FACILITATOR_URL}`,
  );
});
