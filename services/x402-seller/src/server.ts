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
import { zauthProvider } from "@zauthx402/sdk/middleware";
import { getPassport } from "./passport.js";
import { Attestor, ATTEST_DOMAIN, ATTEST_CANONICALIZATION, ATTEST_VERIFY_RECIPE } from "./attest.js";

const PORT = Number(process.env.PORT ?? 10000);
const PAY_TO = process.env.COVENANT_TREASURY ?? "8xbXHAhiVe2BrYDq4qpTA5SSYJG9XNjNN6jcrudhTKCM";
const FACILITATOR_URL = process.env.X402_FACILITATOR_URL ?? "https://facilitator.payai.network";
const FEE_PAYER = process.env.FACILITATOR_PUBKEY ?? "2wKupLR9q6wXYppw8Gr2NvWxKBUqm4PPJKkQfoxHDBg4";
const SYNC = process.env.X402_SYNC_FACILITATOR !== "false";
const ZAUTH_API_KEY = process.env.ZAUTH_API_KEY;
const RPC_URL = process.env.COVENANT_SOLANA_MAINNET_RPC_URL ?? "https://api.mainnet-beta.solana.com";
const RPC_TIMEOUT = Number(process.env.RPC_TIMEOUT_MS ?? 9000);

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
  res.json({ ok: true, service: "covenant-x402-seller", resources: ["/x402/passport/:asset", "/x402/attest"] });
});

app.get("/openapi.json", (_req: Request, res: Response) => {
  res.set("cache-control", "public, max-age=300").json(openapi);
});

// x402 discovery — lets crawlers (zauth directory, x402scan) list the resources.
app.get("/.well-known/x402", (req: Request, res: Response) => {
  const base = `${req.protocol}://${req.get("host")}`;
  res.json({
    version: 1,
    resources: [`${base}/x402/passport/{asset}`, `${base}/x402/attest`],
    instructions:
      "Covenant Trust x402 seller. Pay USDC on Solana to verify an agent's on-chain identity passport (GET /x402/passport/<mpl-core-asset>) or to obtain a Covenant-signed attestation over a claim (POST /x402/attest).",
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

const gate = (amount: string, description: string) => ({
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
});

const routes: RoutesConfig = {
  "GET /x402/passport/:asset": gate(
    "1000",
    "Verify an agent's on-chain identity passport: MPL Core asset, 014 Registry binding, and Covenant attestation.",
  ),
  "POST /x402/attest": gate("5000", "Create a Covenant-signed ed25519 attestation over a claim."),
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

app.listen(PORT, () => {
  console.log(
    `covenant-x402-seller on :${PORT} — paid /x402/passport/:asset + /x402/attest, payTo ${PAY_TO}, facilitator ${FACILITATOR_URL}`,
  );
});
