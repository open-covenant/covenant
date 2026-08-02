/**
 * Covenant Trust x402 seller on Base.
 *
 * Agents pay per call in USDC (EIP-3009 transferWithAuthorization) to obtain a
 * Covenant-signed ed25519 attestation over a claim:
 *   POST /x402/attest                    a Covenant-signed attestation over a { subject, claim } pair.
 *   POST /x402/robinhood/governed-order  place a Robinhood order through the Covenant policy gate.
 *   GET  /x402/proof/:agent/trading      an agent's verifiable trading reputation.
 *
 * `@x402/express` issues the 402 challenge via a locally-registered (signer-less)
 * EVM exact scheme, then verifies and settles through the Coinbase-hosted x402
 * facilitator, which pays L2 gas and settles the USDC transfer on Base. Revenue
 * lands at payTo. If the facilitator cannot settle, the middleware fails closed:
 * the buyer is never charged and the resource is never released. A handler that
 * returns >= 400 also cancels settlement, so a malformed request is free.
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
 *   COVENANT_HTTP_URL       covenantd gateway for the robinhood routes (default 127.0.0.1:8421)
 *   COVENANT_AUTH_TOKEN     bearer for covenantd
 */
import express, { type Request, type Response } from "express";
import { paymentMiddlewareFromConfig } from "@x402/express";
import { HTTPFacilitatorClient, type RoutesConfig } from "@x402/core/server";
import { createFacilitatorConfig } from "@coinbase/x402";
import { ExactEvmScheme } from "@x402/evm/exact/server";
import { declareDiscoveryExtension } from "@x402/extensions/bazaar";
import { Attestor, ATTEST_DOMAIN, ATTEST_CANONICALIZATION, ATTEST_VERIFY_RECIPE } from "./attest.js";
import { makeAttestHandler } from "./attest-route.js";
import { makeGovernedOrderHandler, makeReputationHandler } from "./robinhood-route.js";

// The EIP-712 domain (name, version) is the token's own, not ours: the buyer's
// wallet signs the transferWithAuthorization against it and the facilitator
// verifies the signature against the on-chain contract. It differs per network,
// so advertising the wrong pair would make every real payment fail verification.
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
// Coinbase's CDP facilitator settles fee-free USDC on Base mainnet but requires
// CDP credentials on every call, including the supported-kinds fetch. Base
// Sepolia has no such gate, so default to the public facilitator there: it lets
// the service boot and settle testnet payments without credentials. Override
// either with X402_FACILITATOR_URL.
const FACILITATOR_URL =
  process.env.X402_FACILITATOR_URL ??
  (NET === "base" ? "https://api.cdp.coinbase.com/platform/v2/x402" : "https://x402.org/facilitator");
// The resource server builds the 402 challenge from the facilitator's advertised
// supported kinds, so this fetch has to run before the first challenge can be
// issued. Leave it on unless you are pointing at a facilitator that cannot be
// reached at boot.
const SYNC = process.env.X402_SYNC_FACILITATOR !== "false";
const PRICE = "10000"; // 0.01 USDC, base units (USDC is 6 decimals)

if (!/^0x[0-9a-fA-F]{40}$/.test(PAY_TO)) {
  console.error("COVENANT_BASE_PAYTO is required and must be a 0x-prefixed 40-hex EVM address");
  process.exit(1);
}

const attestor = process.env.COVENANT_ATTEST_KEYPAIR
  ? new Attestor(JSON.parse(process.env.COVENANT_ATTEST_KEYPAIR) as number[])
  : Attestor.generate();
if (!process.env.COVENANT_ATTEST_KEYPAIR) {
  console.warn(`COVENANT_ATTEST_KEYPAIR unset, generated ephemeral attestation key ${attestor.pubkeyB58}`);
}

const RESOURCES = ["/x402/attest", "/x402/robinhood/governed-order", "/x402/proof/:agent/trading"];

const app = express();
// Render terminates TLS and forwards over http; trust the proxy so req.protocol
// reflects the real https scheme in the discovery URLs.
app.set("trust proxy", true);
app.use(express.json());

app.get("/health", (_req: Request, res: Response) => {
  res.json({
    ok: true,
    service: "covenant-x402-seller-base",
    network: NET,
    chain: net.chain,
    asset: ASSET,
    payTo: PAY_TO,
    resources: RESOURCES,
  });
});

app.get("/.well-known/x402", (req: Request, res: Response) => {
  const base = `${req.protocol}://${req.get("host")}`;
  res.json({
    version: 1,
    resources: RESOURCES.map((r) => `${base}${r}`),
    instructions:
      "Covenant Trust x402 seller on Base. Pay USDC via EIP-3009 to obtain a Covenant-signed ed25519 attestation (POST /x402/attest), place a policy-gated Robinhood order (POST /x402/robinhood/governed-order), or fetch an agent's trading reputation (GET /x402/proof/:agent/trading). Pin the attestation key below to verify responses without trusting this server.",
    attestation: {
      algorithm: "ed25519",
      publicKey: attestor.pubkeyB58,
      domain: ATTEST_DOMAIN,
      canonicalization: ATTEST_CANONICALIZATION,
      verify: ATTEST_VERIFY_RECIPE,
    },
  });
});

// CDP credentials authenticate every facilitator call and pin the URL to
// Coinbase's Base facilitator, which is what mainnet settlement requires. Absent
// them we use the plain URL, which is all the Base Sepolia public facilitator
// needs. createFacilitatorConfig signs an ES256 CDP JWT per request.
const CDP_ID = process.env.CDP_API_KEY_ID;
const CDP_SECRET = process.env.CDP_API_KEY_SECRET;
const facilitatorConfig =
  CDP_ID && CDP_SECRET ? createFacilitatorConfig(CDP_ID, CDP_SECRET) : { url: FACILITATOR_URL };
const facilitator = new HTTPFacilitatorClient(facilitatorConfig);

// `extensions` carries the bazaar discovery declaration so the resource is
// listed in facilitator-backed catalogs (x402scan and the like). Without it the
// route still settles but stays invisible to discovery crawlers.
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
  tags: ["covenant", "trust", "attestation", "agent", "base"],
  extensions,
});

const routes: RoutesConfig = {
  "POST /x402/attest": gate(
    PRICE,
    "Create a Covenant-signed ed25519 attestation over a claim.",
    declareDiscoveryExtension({
      input: { subject: "0x5fA1d0C0bfFE257a20027C523093F941834f5D66", claim: { delivered: true } },
      inputSchema: {
        properties: { subject: { type: "string" }, claim: {} },
        required: ["subject", "claim"],
      },
      bodyType: "json",
      output: { example: { alg: "ed25519", signature_b58: "...", pubkey_b58: "..." } },
    }),
  ),
  "POST /x402/robinhood/governed-order": gate(
    PRICE,
    "Place a Robinhood crypto order through the Covenant policy gate; returns the signed, on-chain-anchored trade receipt.",
    declareDiscoveryExtension({
      input: { symbol: "BTC-USD", side: "buy", type: "market", quantity: 0.001 },
      inputSchema: {
        properties: {
          symbol: { type: "string" },
          side: { type: "string" },
          type: { type: "string" },
          quantity: { type: "number" },
          limit_price: { type: "number" },
        },
        required: ["symbol", "side", "quantity"],
      },
      bodyType: "json",
      output: { example: { receipt: { decision: "executed" }, root_hash_hex: "...", signature_b64: "...", anchor: "..." } },
    }),
  ),
  "GET /x402/proof/:agent/trading": gate(
    PRICE,
    "Fetch an agent's verifiable trading reputation derived from its Covenant trade receipts.",
    declareDiscoveryExtension({
      inputSchema: { properties: {} },
      output: { example: { agent: "robinhood-agent", score: 100, established: true, mandate_adherence: 1 } },
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

// Paid, reached only after a verified payment. Returning >= 400 cancels
// settlement, so a bad request is never charged. Handlers live in their route
// modules so tests can hit them without the payment middleware.
app.post("/x402/attest", makeAttestHandler(attestor));
app.post("/x402/robinhood/governed-order", makeGovernedOrderHandler());
app.get("/x402/proof/:agent/trading", makeReputationHandler());

app.listen(PORT, () => {
  console.log(
    `covenant-x402-seller-base on :${PORT}, paid ${RESOURCES.join(" + ")}, ${NET} (${net.chain}) USDC ${ASSET}, payTo ${PAY_TO}, facilitator ${facilitatorConfig.url ?? FACILITATOR_URL}${CDP_ID && CDP_SECRET ? " (cdp-authed)" : ""}`,
  );
});
