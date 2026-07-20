/**
 * Covenant BlockRun-receipt verify seller, on Base.
 *
 * Agents and organizations pay per call in USDC (EIP-3009 transferWithAuthorization)
 * to have Covenant verify a BlockRun call receipt:
 *   POST /x402/verify  recompute the receipt's JCS digest, check the stated
 *                      verdict against the requested/served model pair, and note
 *                      whether it carries a settlement tx — returned with a
 *                      Covenant-signed ed25519 attestation over the result.
 *
 * `@x402/express` issues the 402 challenge via a locally-registered (signer-less)
 * EVM exact scheme, then verifies and settles through the Coinbase-hosted x402
 * facilitator. Revenue lands at payTo. The middleware fails closed: the buyer is
 * never charged and the resource is never released if settlement fails, and a
 * handler returning >= 400 also cancels settlement, so a malformed receipt is free.
 *
 * This is Covenant dogfooding x402: a paid trust service on the same rail the
 * receipts it verifies were settled over.
 *
 * Env (Render vars):
 *   PORT                    listen port (Render injects it)
 *   X402_NETWORK            base-sepolia (default), base, eip155:8453, eip155:84532
 *   X402_ASSET              USDC contract for the network (defaults per network)
 *   COVENANT_BASE_PAYTO     payTo, the 0x address USDC revenue lands at (required)
 *   X402_FACILITATOR_URL    facilitator base (verify + settle); network-aware default
 *   CDP_API_KEY_ID          Coinbase CDP key id, enables authed Base mainnet settle
 *   CDP_API_KEY_SECRET      Coinbase CDP key secret
 *   X402_SYNC_FACILITATOR   "false" to skip the boot supported-kinds fetch (default on)
 *   COVENANT_ATTEST_KEYPAIR 64-byte JSON array seed; an ephemeral key is generated
 *                           and its pubkey logged when this is unset
 *   BLOCKRUN_VERIFY_PRICE   price in USDC base units (default 5000 = 0.005 USDC)
 */
import express, { type Request, type Response } from "express";
import { paymentMiddlewareFromConfig } from "@x402/express";
import { HTTPFacilitatorClient, type RoutesConfig } from "@x402/core/server";
import { createFacilitatorConfig } from "@coinbase/x402";
import { ExactEvmScheme } from "@x402/evm/exact/server";
import { declareDiscoveryExtension } from "@x402/extensions/bazaar";
import { Attestor, ATTEST_DOMAIN, ATTEST_CANONICALIZATION, ATTEST_VERIFY_RECIPE } from "./attest.js";
import { makeVerifyHandler } from "./verify-route.js";

const NETWORKS = {
  base: {
    chain: "eip155:8453",
    usdc: "0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913",
    domain: { name: "USD Coin", version: "2" },
  },
  "base-sepolia": {
    chain: "eip155:84532",
    usdc: "0x036CbD53842c5426634e7929541eC2318f3dCF7e",
    domain: { name: "USDC", version: "2" },
  },
} as const;

type NetKey = keyof typeof NETWORKS;

const NET_ALIASES: Record<string, NetKey> = {
  base: "base",
  "eip155:8453": "base",
  "8453": "base",
  "base-sepolia": "base-sepolia",
  "eip155:84532": "base-sepolia",
  "84532": "base-sepolia",
};

function resolveNetwork(raw: string | undefined): NetKey {
  const key = NET_ALIASES[(raw ?? "base-sepolia").trim().toLowerCase()];
  if (!key) {
    console.error(`X402_NETWORK must be base, base-sepolia, eip155:8453, or eip155:84532 (got "${raw}")`);
    process.exit(1);
  }
  return key;
}

const PORT = Number(process.env.PORT ?? 10000);
const NET = resolveNetwork(process.env.X402_NETWORK);
const net = NETWORKS[NET];
const ASSET = process.env.X402_ASSET ?? net.usdc;
const PAY_TO = process.env.COVENANT_BASE_PAYTO ?? "";
const FACILITATOR_URL =
  process.env.X402_FACILITATOR_URL ??
  (NET === "base" ? "https://api.cdp.coinbase.com/platform/v2/x402" : "https://x402.org/facilitator");
const SYNC = process.env.X402_SYNC_FACILITATOR !== "false";
const PRICE = process.env.BLOCKRUN_VERIFY_PRICE ?? "5000"; // 0.005 USDC (6 decimals)

if (!/^0x[0-9a-fA-F]{40}$/.test(PAY_TO)) {
  console.error("COVENANT_BASE_PAYTO is required and must be a 0x-prefixed 40-hex EVM address");
  process.exit(1);
}

if (!/^[1-9][0-9]*$/.test(PRICE)) {
  console.error(
    `BLOCKRUN_VERIFY_PRICE must be a positive integer in USDC base units (got "${PRICE}"); 5000 = 0.005 USDC`,
  );
  process.exit(1);
}

const attestor = process.env.COVENANT_ATTEST_KEYPAIR
  ? new Attestor(JSON.parse(process.env.COVENANT_ATTEST_KEYPAIR) as number[])
  : Attestor.generate();
if (!process.env.COVENANT_ATTEST_KEYPAIR) {
  console.warn(`COVENANT_ATTEST_KEYPAIR unset, generated ephemeral attestation key ${attestor.pubkeyB58}`);
}

const app = express();
app.set("trust proxy", true);
app.use(express.json({ limit: "256kb" }));

app.get("/health", (_req: Request, res: Response) => {
  res.json({
    ok: true,
    service: "covenant-blockrun-verify",
    network: NET,
    chain: net.chain,
    asset: ASSET,
    payTo: PAY_TO,
    resources: ["/x402/verify"],
  });
});

app.get("/.well-known/x402", (req: Request, res: Response) => {
  const base = `${req.protocol}://${req.get("host")}`;
  res.json({
    version: 1,
    resources: [`${base}/x402/verify`],
    instructions:
      "Covenant BlockRun-receipt verifier. Pay USDC via EIP-3009 to verify a BlockRun call receipt (POST /x402/verify with { receipt, expectedDigest? }): the response recomputes the receipt's JCS digest, checks the stated verdict against the requested/served model pair, and returns a Covenant-signed ed25519 attestation over the result. Pin the attestation key below to verify responses without trusting this server.",
    attestation: {
      algorithm: "ed25519",
      publicKey: attestor.pubkeyB58,
      domain: ATTEST_DOMAIN,
      canonicalization: ATTEST_CANONICALIZATION,
      verify: ATTEST_VERIFY_RECIPE,
    },
  });
});

const CDP_ID = process.env.CDP_API_KEY_ID;
const CDP_SECRET = process.env.CDP_API_KEY_SECRET;
const facilitatorConfig =
  CDP_ID && CDP_SECRET ? createFacilitatorConfig(CDP_ID, CDP_SECRET) : { url: FACILITATOR_URL };
const facilitator = new HTTPFacilitatorClient(facilitatorConfig);

// The 402 challenge is built from the facilitator's advertised supported kinds.
// With the boot sync off and no CDP credentials to authenticate an on-demand
// fetch, the middleware never learns it can issue a challenge and throws 500 on
// every unpaid request. Warn loudly rather than fail silently at request time.
if (!SYNC && !(CDP_ID && CDP_SECRET)) {
  console.warn(
    "X402_SYNC_FACILITATOR=false with no CDP credentials: the facilitator's supported kinds are never fetched, so unpaid requests will 500 instead of returning a 402. Leave sync on (the default) or provide CDP_API_KEY_ID/SECRET.",
  );
}

const gate = (amount: string, description: string, extensions: Record<string, unknown>) => ({
  accepts: {
    scheme: "exact" as const,
    network: net.chain,
    payTo: PAY_TO,
    price: { asset: ASSET, amount, extra: { ...net.domain } },
    maxTimeoutSeconds: 300,
  },
  description,
  mimeType: "application/json",
  serviceName: "Covenant",
  tags: ["covenant", "trust", "blockrun", "receipt", "verify", "base"],
  extensions,
});

const routes: RoutesConfig = {
  "POST /x402/verify": gate(
    PRICE,
    "Verify a BlockRun call receipt and return a Covenant-signed result.",
    declareDiscoveryExtension({
      input: {
        receipt: {
          provider: "blockrun",
          endpoint: "/v1/chat/completions",
          modelRequested: "gpt-4o",
          modelServed: "gpt-4o-mini",
          verdict: "substituted",
          inputSha256: "…",
          outputSha256: "…",
          payment: { network: "eip155:8453", amount: "3000", tx: "0x…" },
        },
      },
      inputSchema: {
        properties: { receipt: { type: "object" }, expectedDigest: { type: "string" } },
        required: ["receipt"],
      },
      bodyType: "json",
      output: {
        example: {
          result: { valid: true, digest: "…", verdictConsistent: true, expectedVerdict: "substituted" },
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
    [{ network: net.chain, server: new ExactEvmScheme() }],
    undefined,
    undefined,
    SYNC,
  ),
);

app.post("/x402/verify", makeVerifyHandler(attestor));

app.listen(PORT, () => {
  console.log(
    `covenant-blockrun-verify on :${PORT} (${NET}, ${net.chain}), price ${PRICE} base units, payTo ${PAY_TO}`,
  );
});
