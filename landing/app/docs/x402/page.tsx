import Link from 'next/link';
import { buildDocsMetadata, buildDocsJsonLd } from '../_meta';

const META_ARGS = [
  'x402',
  'Covenant x402',
  'Pay-per-call USDC over HTTP 402: Covenant agents pay for resources, and Covenant exposes paid resources to other agents.',
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
        x402 is HTTP <code>402 Payment Required</code> turned into a working payment rail. A
        resource answers an unpaid request with a signed price quote, the caller pays on-chain, and
        the same request retried with an <code>X-PAYMENT</code> header returns the resource.
        Covenant uses x402 in both directions: agents pay for external resources, and Covenant
        exposes paid resources to other agents.
      </p>
      <p>
        The deployed inbound seller and outbound signer are separate integration surfaces. The
        seller speaks x402 v2. The reusable outbound Solana signer emits the repository&apos;s
        legacy v1 payment envelope; it does not become v2-compatible because the seller is. The
        daemon-mediated outbound path records capability, budget, and audit decisions, but the
        signer binary itself does not require or consume a spend-authorization decision.
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
              Core types (<code>PaymentRequirements</code>, <code>PaymentExtra</code>), the{' '}
              <code>Signer</code> trait, and <code>PayaiSolanaSigner</code> for Solana settlement.
            </td>
          </tr>
          <tr>
            <td>
              <code>covenant-x402-signer</code>
            </td>
            <td>
              A stdin/stdout sidecar binary: read legacy-v1 <code>PaymentRequirements</code> JSON on
              stdin, get the base64 <code>X-PAYMENT</code> header value on stdout. Moving the key
              into a subprocess keeps it out of the daemon process but is not, by itself, a
              wallet-security or policy-enforcement boundary.
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
          <strong>Facilitator:</strong> PayAI is the fee payer and the settler. It co-signs the
          transaction as <code>feePayer</code> (so it sponsors gas) and submits it.
        </li>
        <li>
          <strong>Roles:</strong> the funder (the paying agent) partial-signs the transfer; the
          facilitator co-signs as fee payer and settles; the recipient (<code>payTo</code>) receives
          the USDC. Funder and recipient are always distinct accounts.
        </li>
        <li>
          <strong>Delivery and settlement are separate:</strong> a <code>402</code>, resource error,
          or timeout does not prove that settlement failed. Do not assume no charge. Inspect the
          facilitator response and confirm the transaction or recipient USDC balance on chain before
          retrying.
        </li>
      </ul>

      <h2>Paying for resources (outbound)</h2>
      <p>
        An agent can reach a paid provider through capability-gated MCP tools. After a matching{' '}
        <code>402</code> and a successful payment-header retry, the daemon debits the local budget,
        writes a local receipt, and records the selected live requirement in the hash-chained audit
        log. Those rows do not prove chain settlement. The legacy capability matches network, asset,
        and maximum amount; it does not yet bind trusted <code>payTo</code>, endpoint, scheme, fee
        payer, or redirects, so this is not production W009 enforcement.
      </p>
      <p>
        The <code>covenant-x402-signer</code> sidecar is the reusable primitive: feed it the{' '}
        <code>PaymentRequirements</code> from any <code>402</code> challenge and it returns the
        header to retry with.
      </p>
      <pre>
        <code>{`export COVENANT_X402_FUNDING_KEYPAIR=/path/to/funder.json
export COVENANT_X402_RPC_URL=https://api.mainnet-beta.solana.com
echo "$payment_requirements_json" | covenant-x402-signer   # -> base64 X-PAYMENT header`}</code>
      </pre>

      <h2>Exposing a paid resource (inbound)</h2>
      <p>
        Covenant runs a public x402-v2 seller at <code>https://x402-seller.opencovenant.org</code>.
        It sells evidence endpoints, all advertised at <code>GET /.well-known/x402</code>:
      </p>
      <pre>
        <code>{`GET  /x402/passport/<mpl-core-asset>   registration and configured-record observations
POST /x402/attest                      a Covenant-signed caller-supplied statement
GET  /x402/payai/reputation/<wallet>   legacy heuristic over bounded PayAI-linked transfers
GET  /x402/er/enclave/<validator>      the seller's signed DCAP-monitor result for a validator quote`}</code>
      </pre>
      <p>
        A valid signature authenticates Covenant as publisher. It does not establish that the signed
        claim is true. Registration and payment observations do not prove real-world identity, x402
        job delivery, quality, reputation, or W009/W011 enforcement. The enclave endpoint still
        trusts the seller implementation, issuer, endpoint selection, and collateral handling;
        optional subject bytes do not prove that an agent record originated inside the enclave.
      </p>
      <p>
        Unpaid, each returns an x402 v2 <code>402</code> challenge: the <code>exact</code> scheme,
        Solana mainnet, USDC, <code>payTo</code> the Covenant treasury, <code>feePayer</code> the
        PayAI sponsor, priced per call (currently $0.001 to $0.01). Pay and retry to receive the
        signed result. Resource delivery and settlement are separate: on an error or timeout, do not
        assume no charge. Inspect the facilitator response and confirm the transaction or recipient
        USDC balance on chain before retrying.
      </p>
      <p>The endpoints are discoverable and monitored:</p>
      <ul>
        <li>
          <code>GET /.well-known/x402</code> advertises them so crawlers (the zauth directory,
          x402scan) can list the resources.
        </li>
        <li>
          They are registered and health-monitored in the{' '}
          <Link href="/zauth">zauth provider hub</Link> on Solana mainnet.
        </li>
      </ul>

      <h2>Related</h2>
      <ul>
        <li>
          <Link href="/zauth">Covenant and zauth</Link>: discovery and health-monitoring for x402
          endpoints.
        </li>
        <li>
          <Link href="/settlement">Settlement</Link>: how Covenant accounts for paid calls.
        </li>
        <li>
          <Link href="/mcp">MCP integration</Link>: how agents reach paid tools.
        </li>
      </ul>
    </>
  );
}
