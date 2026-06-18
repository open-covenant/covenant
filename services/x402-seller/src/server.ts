/**
 * Covenant x402 seller endpoint.
 *
 * A public x402 v2 resource: AI agents pay per call in USDC on Solana for
 * a Covenant agent reputation/attestation lookup. `@x402/express` issues
 * the 402 challenge via a locally-registered (signer-less) SVM server
 * scheme, then verifies + settles through a facilitator client. USDC
 * lands at the treasury payTo. `@zauthx402/sdk` reports telemetry to the
 * zauth provider dashboard; the Bazaar discovery the paywall emits lets
 * zauth's crawler list us in the directory.
 *
 * If the facilitator can't settle, `@x402/express` fails closed: the
 * buyer gets a 402 and is never charged, the resource is never released.
 *
 * Env (see .env / Render vars):
 *   PORT                    listen port (Render injects)
 *   COVENANT_TREASURY       payTo — where USDC revenue lands
 *   X402_FACILITATOR_URL    facilitator base (verify/settle + feePayer)
 *   FACILITATOR_PUBKEY      sponsor feePayer advertised in the challenge
 *   X402_SYNC_FACILITATOR   "false" to skip facilitator sync at boot
 *   ZAUTH_API_KEY           zauth provider key (telemetry; optional)
 *   PRICE_ATOMIC            price in atomic USDC (1000 = $0.001)
 */
import express, { type Request, type Response } from "express";
import { paymentMiddlewareFromConfig } from "@x402/express";
import { HTTPFacilitatorClient, type RoutesConfig } from "@x402/core/server";
import { ExactSvmScheme } from "@x402/svm/exact/server";
import { zauthProvider } from "@zauthx402/sdk/middleware";

const PORT = Number(process.env.PORT ?? 10000);
const PAY_TO = process.env.COVENANT_TREASURY ?? "8xbXHAhiVe2BrYDq4qpTA5SSYJG9XNjNN6jcrudhTKCM";
// PayAI is an x402-v2 facilitator that sponsors Solana mainnet (its
// /supported lists exact on solana:5eykt4Us… with this feePayer). It
// co-signs the sponsor + settles, so no local signing key is needed.
const FACILITATOR_URL = process.env.X402_FACILITATOR_URL ?? "https://facilitator.payai.network";
const FEE_PAYER = process.env.FACILITATOR_PUBKEY ?? "2wKupLR9q6wXYppw8Gr2NvWxKBUqm4PPJKkQfoxHDBg4";
const SYNC = process.env.X402_SYNC_FACILITATOR !== "false";
const ZAUTH_API_KEY = process.env.ZAUTH_API_KEY;
const PRICE_ATOMIC = process.env.PRICE_ATOMIC ?? "1000"; // 1000 = $0.001 (USDC 6dp)

const SOLANA_MAINNET = "solana:5eykt4UsFv8P8NJdTREpY1vzqKqZKvdp" as const;
const USDC_SOLANA = "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v";
const RESOURCE_PATH = "/x402/agent/:pubkey";

const app = express();
app.use(express.json());

// Free liveness check, off the paywall.
app.get("/health", (_req: Request, res: Response) => {
  res.json({ ok: true, service: "covenant-x402-seller", resource: RESOURCE_PATH });
});

// x402 discovery — lets crawlers (zauth directory, x402scan) list this
// endpoint. Off the paywall.
app.get("/.well-known/x402", (req: Request, res: Response) => {
  const base = `${req.protocol}://${req.get("host")}`;
  res.json({
    version: 1,
    resources: [`${base}/x402/agent/{pubkey}`],
    instructions:
      "Covenant x402 seller. GET /x402/agent/<solana-pubkey> returns a 402 x402-v2 challenge; pay USDC on Solana and retry to receive a Covenant agent attestation.",
  });
});

// zauth provider telemetry — observes traffic, never blocks payments.
if (ZAUTH_API_KEY) {
  app.use(zauthProvider(ZAUTH_API_KEY));
} else {
  console.warn("ZAUTH_API_KEY unset — running without zauth provider telemetry");
}

const facilitator = new HTTPFacilitatorClient({ url: FACILITATOR_URL });

const routes: RoutesConfig = {
  [`GET ${RESOURCE_PATH}`]: {
    accepts: {
      scheme: "exact",
      network: SOLANA_MAINNET,
      payTo: PAY_TO,
      price: { asset: USDC_SOLANA, amount: PRICE_ATOMIC },
      maxTimeoutSeconds: 300,
      extra: { feePayer: FEE_PAYER },
    },
    description:
      "Covenant agent reputation/attestation lookup: verified on-chain presence and reputation summary for a Solana agent pubkey.",
    mimeType: "application/json",
    serviceName: "Covenant",
    tags: ["covenant", "reputation", "attestation", "agent"],
  },
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

// Paid resource — reached only after verified payment. Returning >= 400
// cancels settlement, so the buyer is never charged for an error.
app.get(RESOURCE_PATH, (req: Request, res: Response) => {
  const pubkey = req.params.pubkey;
  if (!/^[1-9A-HJ-NP-Za-km-z]{32,44}$/.test(pubkey)) {
    res.status(400).json({ error: "pubkey must be a base58 Solana address" });
    return;
  }
  res.json({
    type: "covenant_agent_attestation_v0",
    chain: "solana",
    agent: pubkey,
    issuer: "covenant",
    issuedAt: new Date().toISOString(),
    note: "v0 attestation surface; reputation fields wire to the Covenant audit/reputation layer next.",
  });
});

app.listen(PORT, () => {
  console.log(
    `covenant-x402-seller on :${PORT} — paid ${RESOURCE_PATH}, payTo ${PAY_TO}, feePayer ${FEE_PAYER}, facilitator ${FACILITATOR_URL}`,
  );
});
