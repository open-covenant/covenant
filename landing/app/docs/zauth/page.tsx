import Link from "next/link";
import { buildDocsMetadata, buildDocsJsonLd } from "../_meta";

const META_ARGS = [
  "zauth",
  "Covenant and zauth",
  "Discovery and health-monitoring for Covenant x402 endpoints through the zauth provider hub.",
] as const;
export const metadata = buildDocsMetadata(...META_ARGS);

export default function ZauthPage() {
  return (
    <>
      <script
        type="application/ld+json"
        dangerouslySetInnerHTML={{ __html: JSON.stringify(buildDocsJsonLd(...META_ARGS)) }}
      />
      <h1>Covenant and zauth</h1>
      <p>
        zauth runs a provider hub for x402: a directory that discovers public
        x402 endpoints, health-checks them, and exposes provider telemetry.
        Covenant integrates with it so the paid resources Covenant exposes are
        discoverable and monitored, and so Covenant agents can read the
        directory when looking for paid services.
      </p>

      <h2>Crate</h2>
      <p>
        <code>covenant-zauth</code> (in <code>agent-os/crates/</code>) is the
        Rust client:
      </p>
      <ul>
        <li>
          <strong>Directory client:</strong> query the zauth provider hub for
          registered x402 endpoints (URL, network, health).
        </li>
        <li>
          <strong>x402 v2 header decoder:</strong> decode the{" "}
          <code>payment-required</code> challenge an x402 resource returns into
          the typed requirements Covenant signs against.
        </li>
        <li>
          <strong>RepoScan request builder:</strong> construct RepoScan requests
          against the zauth verifier for an endpoint under test.
        </li>
      </ul>
      <p>
        The signing path reuses <Link href="/x402">covenant-x402</Link> (
        <code>PayaiSolanaSigner</code>); zauth adds no new key handling.
      </p>

      <h2>Live integration</h2>
      <p>
        Covenant&apos;s public x402 seller (
        <code>https://x402-seller.opencovenant.org</code>, see{" "}
        <Link href="/x402">Covenant x402</Link>) is registered and
        health-monitored in the zauth provider hub on Solana mainnet. The hub
        lists the endpoint, periodically health-checks the registered URL, and
        tracks provider telemetry reported by the provider middleware mounted on
        the seller.
      </p>
      <p>
        One operational detail worth stating plainly: the hub registers a
        selling endpoint only once it processes a real settled payment, not on
        SDK install or on <code>402</code> challenge telemetry. An endpoint
        appears in the directory after its first successful on-chain settlement,
        and drops out when its health check stops passing.
      </p>

      <h2>Related</h2>
      <ul>
        <li>
          <Link href="/x402">Covenant x402</Link>: the payment layer zauth
          discovers and monitors.
        </li>
        <li>
          <Link href="/settlement">Settlement</Link>: how Covenant accounts for
          paid calls.
        </li>
      </ul>
    </>
  );
}
