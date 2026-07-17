/**
 * Covenant Trust — public x402 v2 seller.
 *
 * Agents pay per call in USDC on Solana to verify a counterparty before they
 * transact with it:
 *   GET  /x402/passport/:asset  — on-chain identity passport (MPL Core asset +
 *                                 014 Registry binding + Covenant attestation).
 *   POST /x402/attest           — a Covenant-signed, independently-verifiable
 *                                 ed25519 attestation over a claim.
 *
 * `@x402/express` issues the 402 challenge via a locally-registered (signer-less)
 * SVM scheme, then verifies + settles through the PayAI facilitator (which
 * sponsors the Solana fee payer). USDC lands at the treasury payTo. If the
 * facilitator can't settle, the middleware fails closed — the buyer is never
 * charged and the resource is never released. Handlers that return >= 400 also
 * cancel settlement, so an unknown asset or a bad request is free.
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
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import express, { type Request, type Response } from "express";
import { paymentMiddlewareFromConfig } from "@x402/express";
import { HTTPFacilitatorClient, type RoutesConfig } from "@x402/core/server";
import { ExactSvmScheme } from "@x402/svm/exact/server";
import { declareDiscoveryExtension } from "@x402/extensions/bazaar";
import { zauthProvider } from "@zauthx402/sdk/middleware";
import { getPassport } from "./passport.js";
import { Attestor, ATTEST_DOMAIN, ATTEST_CANONICALIZATION, ATTEST_VERIFY_RECIPE } from "./attest.js";
import { getReputation } from "./reputation.js";
import { verifyErEnclave, ErEnclaveError } from "./er-enclave.js";

const PORT = Number(process.env.PORT ?? 10000);
const PAY_TO = process.env.COVENANT_TREASURY ?? "8xbXHAhiVe2BrYDq4qpTA5SSYJG9XNjNN6jcrudhTKCM";
const FACILITATOR_URL = process.env.X402_FACILITATOR_URL ?? "https://facilitator.payai.network";
const FEE_PAYER = process.env.FACILITATOR_PUBKEY ?? "2wKupLR9q6wXYppw8Gr2NvWxKBUqm4PPJKkQfoxHDBg4";
const SYNC = process.env.X402_SYNC_FACILITATOR !== "false";
const ZAUTH_API_KEY = process.env.ZAUTH_API_KEY;
const RPC_URL = process.env.COVENANT_SOLANA_MAINNET_RPC_URL ?? "https://api.mainnet-beta.solana.com";
const RPC_TIMEOUT = Number(process.env.RPC_TIMEOUT_MS ?? 9000);
const REPUTATION_LIMIT = Number(process.env.REPUTATION_LIMIT ?? 100);

const SOLANA_MAINNET = "solana:5eykt4UsFv8P8NJdTREpY1vzqKqZKvdp" as const;
const USDC_SOLANA = "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v";

const attestor = process.env.COVENANT_ATTEST_KEYPAIR
  ? new Attestor(JSON.parse(process.env.COVENANT_ATTEST_KEYPAIR) as number[])
  : null;

const openapi = JSON.parse(
  readFileSync(join(dirname(fileURLToPath(import.meta.url)), "..", "openapi.json"), "utf8"),
);

const app = express();
// Render terminates TLS and forwards over http; trust the proxy so req.protocol
// reflects the real https scheme in discovery URLs.
app.set("trust proxy", true);
app.use(express.json());

app.get("/health", (_req: Request, res: Response) => {
  res.json({
    ok: true,
    service: "covenant-x402-seller",
    resources: ["/x402/passport/:asset", "/x402/attest", "/x402/payai/reputation/:wallet", "/x402/er/enclave/:validator"],
  });
});

app.get("/openapi.json", (_req: Request, res: Response) => {
  res.set("cache-control", "public, max-age=300").json(openapi);
});

// x402 discovery — lets crawlers (zauth directory, x402scan) list the resources.
app.get("/.well-known/x402", (req: Request, res: Response) => {
  const base = `${req.protocol}://${req.get("host")}`;
  res.json({
    version: 1,
    resources: [
      `${base}/x402/passport/{asset}`,
      `${base}/x402/attest`,
      `${base}/x402/payai/reputation/{wallet}`,
      `${base}/x402/er/enclave/{validator}`,
    ],
    instructions:
      "Covenant Trust x402 seller. Pay USDC on Solana to verify an agent's on-chain identity passport (GET /x402/passport/<mpl-core-asset>), obtain a Covenant-signed attestation over a claim (POST /x402/attest), read a wallet's PayAI settlement-grounded reputation (GET /x402/payai/reputation/<wallet>), or get a Covenant-signed live TDX verification of a MagicBlock ephemeral rollup (GET /x402/er/enclave/<validator>, optional ?agent=<pubkey>&provenance_root=<hex> to bind an agent's record into the quote challenge).",
    // Pin this key to verify /x402/attest responses without trusting this server.
    attestation: attestor
      ? {
          algorithm: "ed25519",
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
  console.warn("ZAUTH_API_KEY unset — running without zauth provider telemetry");
}

const facilitator = new HTTPFacilitatorClient({ url: FACILITATOR_URL });

// `extensions` carries the bazaar discovery declaration so each resource is
// listed in the facilitator's discovery catalog (x402scan, the PayAI bazaar).
// Without it the routes still settle but stay invisible to discovery crawlers.
const gate = (amount: string, description: string, extensions: Record<string, unknown>) => ({
  accepts: {
    scheme: "exact" as const,
    network: SOLANA_MAINNET,
    payTo: PAY_TO,
    price: { asset: USDC_SOLANA, amount },
    maxTimeoutSeconds: 300,
    extra: { feePayer: FEE_PAYER },
  },
  description,
  mimeType: "application/json",
  serviceName: "Covenant",
  tags: ["covenant", "trust", "identity", "attestation", "agent"],
  extensions,
});

const routes: RoutesConfig = {
  "GET /x402/passport/:asset": gate(
    "1000",
    "Verify an agent's on-chain identity passport: MPL Core asset, 014 Registry binding, and Covenant attestation.",
    declareDiscoveryExtension({
      pathParams: { asset: "9sFJ95mZsBTGqTEBkcbmsx2V8RQiZ5iQACCLPLE61aWH" },
      pathParamsSchema: {
        properties: { asset: { type: "string", description: "MPL Core asset address (base58)" } },
        required: ["asset"],
      },
      output: {
        example: {
          asset: { id: "9sFJ95mZsBTGqTEBkcbmsx2V8RQiZ5iQACCLPLE61aWH", inCovenantCollection: true },
          registry: { registered: true },
          attestation: { covenantAuthored: true },
        },
      },
    }),
  ),
  "POST /x402/attest": gate(
    "5000",
    "Create a Covenant-signed ed25519 attestation over a claim.",
    declareDiscoveryExtension({
      input: { subject: "9sFJ95mZsBTGqTEBkcbmsx2V8RQiZ5iQACCLPLE61aWH", claim: { delivered: true } },
      inputSchema: {
        properties: { subject: { type: "string" }, claim: {} },
        required: ["subject", "claim"],
      },
      bodyType: "json",
      output: { example: { alg: "ed25519", signature_b58: "…", pubkey_b58: "…" } },
    }),
  ),
  "GET /x402/payai/reputation/:wallet": gate(
    "3000",
    "Read a wallet's PayAI settlement-grounded reputation: settled jobs, distinct counterparties, volume, tier, and a 0-1000 score, computed from on-chain USDC settlements and Covenant-signed.",
    declareDiscoveryExtension({
      pathParams: { wallet: "CvX23FNQsNQww8ALR3EWgQv2Wt5yM7VvU4HRigGBMMJu" },
      pathParamsSchema: {
        properties: { wallet: { type: "string", description: "Solana wallet (owner) address (base58)" } },
        required: ["wallet"],
      },
      output: {
        example: {
          reputation: {
            wallet: "CvX23FNQsNQww8ALR3EWgQv2Wt5yM7VvU4HRigGBMMJu",
            settled_jobs: 115,
            distinct_counterparties: 78,
            volume_micro_usdc: 117000,
            tier: "bronze",
            score: 600,
          },
          attestation: { alg: "ed25519", signature_b58: "…", pubkey_b58: "…" },
        },
      },
    }),
  ),
  "GET /x402/er/enclave/:validator": gate(
    "10000",
    "Live TDX verification of a MagicBlock ephemeral rollup: a fresh quote from the validator's router-listed endpoint, DCAP-verified against the Intel PCCS, Covenant-signed. Optionally binds an agent and its provenance root into the quote challenge.",
    declareDiscoveryExtension({
      pathParams: { validator: "MTEWGuqxUpYZGFJQcp8tLN7x5v9BSeoFHYWQQ3n3xzo" },
      pathParamsSchema: {
        properties: { validator: { type: "string", description: "ER validator identity from the Magic Router (base58)" } },
        required: ["validator"],
      },
      output: {
        example: {
          enclave: { validator: "MTEWGuqxUpYZGFJQcp8tLN7x5v9BSeoFHYWQQ3n3xzo", tee: "intel-tdx", status: "UpToDate", mr_td: "…" },
          attestation: { alg: "ed25519", signature_b58: "…", pubkey_b58: "…" },
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

// Paid — reached only after verified payment. Returning >= 400 cancels
// settlement, so an unknown asset or bad request is never charged.
app.get("/x402/passport/:asset", async (req: Request, res: Response) => {
  try {
    const { status, body } = await getPassport(RPC_URL, RPC_TIMEOUT, req.params.asset);
    res.status(status).json(body);
  } catch {
    res.status(502).json({ error: "chain/DAS upstream unavailable" });
  }
});

app.post("/x402/attest", (req: Request, res: Response) => {
  if (!attestor) {
    res.status(503).json({ error: "attestation signer not configured" });
    return;
  }
  const { subject, claim } = (req.body ?? {}) as { subject?: unknown; claim?: unknown };
  if (typeof subject !== "string" || !subject || subject.length > 256 || claim === undefined) {
    res.status(400).json({ error: "subject (1–256 char string) and claim are required" });
    return;
  }
  res.json(attestor.attest(subject, claim, Math.floor(Date.now() / 1000)));
});

// Paid — a wallet's settlement-grounded reputation, signed with the same
// published attestation key. Returning >= 400 cancels settlement, so an RPC
// failure or a bad address is never charged.
app.get("/x402/payai/reputation/:wallet", async (req: Request, res: Response) => {
  if (!attestor) {
    res.status(503).json({ error: "attestation signer not configured" });
    return;
  }
  const wallet = req.params.wallet;
  if (!/^[1-9A-HJ-NP-Za-km-z]{32,44}$/.test(wallet)) {
    res.status(400).json({ error: "wallet must be a base58 Solana address" });
    return;
  }
  try {
    const reputation = await getReputation(RPC_URL, RPC_TIMEOUT, wallet, REPUTATION_LIMIT);
    const attestation = attestor.attest(wallet, reputation, Math.floor(Date.now() / 1000));
    res.json({ reputation, attestation });
  } catch {
    res.status(502).json({ error: "chain/RPC upstream unavailable" });
  }
});

// Paid — a live TDX check of a MagicBlock ER, wrapped in the same published
// attestation key. Returning >= 400 cancels settlement, so an open validator,
// a bad binding, or an upstream failure is never charged.
app.get("/x402/er/enclave/:validator", async (req: Request, res: Response) => {
  if (!attestor) {
    res.status(503).json({ error: "attestation signer not configured" });
    return;
  }
  const validator = req.params.validator;
  if (!/^[1-9A-HJ-NP-Za-km-z]{32,44}$/.test(validator)) {
    res.status(400).json({ error: "validator must be a base58 identity from the Magic Router" });
    return;
  }
  const agent = typeof req.query.agent === "string" ? req.query.agent : undefined;
  const provenanceRoot = typeof req.query.provenance_root === "string" ? req.query.provenance_root : undefined;
  if ((agent === undefined) !== (provenanceRoot === undefined)) {
    res.status(400).json({ error: "agent and provenance_root bind together — pass both or neither" });
    return;
  }
  if (agent && !/^[1-9A-HJ-NP-Za-km-z]{32,44}$/.test(agent)) {
    res.status(400).json({ error: "agent must be a base58 Solana address" });
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
    res.status(502).json({ error: "enclave verification upstream unavailable" });
  }
});

app.listen(PORT, () => {
  console.log(
    `covenant-x402-seller on :${PORT} — paid /x402/passport/:asset + /x402/attest + /x402/payai/reputation/:wallet + /x402/er/enclave/:validator, payTo ${PAY_TO}, facilitator ${FACILITATOR_URL}`,
  );
});
