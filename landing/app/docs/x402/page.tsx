import Link from "next/link";
import { buildDocsMetadata, buildDocsJsonLd } from "../_meta";

const META_ARGS = [
  "x402",
  "Covenant x402",
  "Pay-per-call USDC over HTTP 402: Covenant agents pay for resources, and Covenant exposes paid resources to other agents.",
] as const;
export const metadata = buildDocsMetadata(...META_ARGS);

export default function X402Page() {
  return (
    <>
      <script
        type="application/ld+json"
        dangerouslySetInnerHTML={{ __html: JSON.stringify(buildDocsJsonLd(...META_ARGS)) }}
      />
      <h1>Covenant x402</h1>
      <p>
        x402 is HTTP <code>402 Payment Required</code> turned into a working
        payment rail. A resource answers an unpaid request with a signed price
        quote, the caller pays on-chain, and the same request retried with an{" "}
        <code>X-PAYMENT</code> header returns the resource. Covenant uses x402 in
        both directions: agents pay for external resources, and Covenant exposes
        paid resources to other agents.
      </p>
      <p>
        Both directions run through one accounting path. Settlement, budgeting,
        audit, and attestation are shared, so a paid call is a first-class,
        capability-gated, audited action rather than a side channel.
      </p>

      <h2>Crates</h2>
      <table>
        <thead>
          <tr>
            <th>Crate</th>
            <th>Role</th>
          </tr>
        </thead>
        <tbody>
          <tr>
            <td>
              <code>covenant-x402</code>
            </td>
            <td>
              Core types (<code>PaymentRequirements</code>,{" "}
              <code>PaymentExtra</code>), the <code>Signer</code> trait, and{" "}
              <code>PayaiSolanaSigner</code> for Solana settlement.
            </td>
          </tr>
          <tr>
            <td>
              <code>covenant-x402-signer</code>
            </td>
            <td>
              A stdin/stdout sidecar binary: read a{" "}
              <code>PaymentRequirements</code> JSON on stdin, get the base64{" "}
              <code>X-PAYMENT</code> header value on stdout. Keeps signing out of
              process.
            </td>
          </tr>
        </tbody>
      </table>

      <h2>Money path (Solana)</h2>
      <ul>
        <li>
          <strong>Network and asset:</strong> Solana mainnet, USDC (
          <code>EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v</code>).
        </li>
        <li>
          <strong>Facilitator:</strong> PayAI is the fee payer and the settler.
          It co-signs the transaction as <code>feePayer</code> (so it sponsors
          gas) and submits it.
        </li>
        <li>
          <strong>Roles:</strong> the funder (the paying agent) partial-signs
          the transfer; the facilitator co-signs as fee payer and settles; the
          recipient (<code>payTo</code>) receives the USDC. Funder and recipient
          are always distinct accounts.
        </li>
        <li>
          <strong>Fail closed:</strong> if the facilitator cannot settle, the
          caller receives a <code>402</code> and is never charged, and the
          resource is never released.
        </li>
      </ul>

      <h2>Paying for resources (outbound)</h2>
      <p>
        An agent reaches a paid provider through capability-gated MCP tools. The
        daemon signs the x402 payment with the agent funding key, debits the
        agent budget, writes a settlement receipt, and records the call in the
        hash-chained audit log. The agent never holds raw payment plumbing; it
        calls a tool and gets a result.
      </p>
      <p>
        The <code>covenant-x402-signer</code> sidecar is the reusable primitive:
        feed it the <code>PaymentRequirements</code> from any <code>402</code>{" "}
        challenge and it returns the header to retry with.
      </p>
      <pre>
        <code>{`export COVENANT_X402_FUNDING_KEYPAIR=/path/to/funder.json
export COVENANT_X402_RPC_URL=https://api.mainnet-beta.solana.com
echo "$payment_requirements_json" | covenant-x402-signer   # -> base64 X-PAYMENT header`}</code>
      </pre>

      <h2>Exposing a paid resource (inbound)</h2>
      <p>
        Covenant runs a public x402 seller at{" "}
        <code>https://x402-seller.opencovenant.org</code>. It sells a Covenant
        agent reputation and attestation lookup:
      </p>
      <pre>
        <code>{`GET /x402/agent/<solana-pubkey>`}</code>
      </pre>
      <p>
        Unpaid, it returns an x402 v2 <code>402</code> challenge: the{" "}
        <code>exact</code> scheme, Solana mainnet, USDC, <code>payTo</code> the
        Covenant treasury, <code>feePayer</code> the PayAI sponsor, price $0.001.
        Pay and retry, and it returns a{" "}
        <code>covenant_agent_attestation_v0</code> object for that pubkey.
        Returning a status of 400 or higher from the resource cancels
        settlement, so a caller is never charged for an error.
      </p>
      <p>The endpoint is discoverable and monitored:</p>
      <ul>
        <li>
          <code>GET /.well-known/x402</code> advertises the resource so crawlers
          (the zauth directory, x402scan) can list it.
        </li>
        <li>
          It is registered and health-monitored in the{" "}
          <Link href="/zauth">zauth provider hub</Link> on Solana mainnet.
        </li>
      </ul>
      <p>
        The v0 attestation surface returns verified on-chain presence today;
        richer reputation fields wire to the Covenant audit and reputation layer
        next.
      </p>

      <h2>Related</h2>
      <ul>
        <li>
          <Link href="/zauth">Covenant and zauth</Link>: discovery and
          health-monitoring for x402 endpoints.
        </li>
        <li>
          <Link href="/settlement">Settlement</Link>: how Covenant accounts for
          paid calls.
        </li>
        <li>
          <Link href="/mcp">MCP integration</Link>: how agents reach paid tools.
        </li>
      </ul>
    </>
  );
}
