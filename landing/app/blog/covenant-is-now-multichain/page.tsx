import type { Metadata } from "next";
import { SiteFooter } from "../../SiteFooter";
import { SiteHeader } from "../../SiteHeader";

export const metadata: Metadata = {
  title: "Covenant is now multi-chain. The token isn't.",
  description:
    "Selected Covenant registrations and signed statements are readable on Base while $CVNT stays on Solana. ecrecover proves only that a configured address signed the bytes, not publisher identity or claim truth.",
  alternates: { canonical: "/blog/covenant-is-now-multichain" },
  openGraph: {
    type: "article",
    url: "https://opencovenant.org/blog/covenant-is-now-multichain",
    title: "Covenant is now multi-chain. The token isn't.",
    description:
      "Selected registrations and statements signed under configured keys are readable on Base while $CVNT stays on Solana.",
    images: [
      {
        url: "/og/covenant-is-now-multichain.png",
        width: 1200,
        height: 630,
        alt: "Covenant is going multi-chain. The token isn't.",
      },
    ],
  },
  twitter: {
    card: "summary_large_image",
    site: "@OpenCovenant",
    creator: "@OpenCovenant",
    title: "Covenant is now multi-chain. The token isn't.",
    description: "Signed evidence reaches Base. The token stays on Solana.",
    images: "/og/covenant-is-now-multichain.png",
  },
};

const backLink =
  "font-mono text-[12px] uppercase tracking-[0.2em] text-neutral-400 transition-colors hover:text-neutral-100";

export default function MultichainLaunchPost() {
  return (
    <main id="main-content" className="relative min-h-screen overflow-x-hidden bg-[#030303]">
      <SiteHeader />

      <div className="page-container">
        <a href="/blog" className={backLink}>
          ← Blog
        </a>

        <p className="mt-10 font-mono text-[11px] uppercase tracking-[0.3em] text-neutral-400">
          Protocol · 7 July 2026
        </p>

        <h1 className="mt-5 max-w-3xl text-balance text-[2.2rem] font-extralight uppercase leading-[1.1] tracking-[2px] text-white sm:text-[2.6rem]">
          Covenant is now multi-chain. The token isn&apos;t.
        </h1>

        <p className="mt-6 max-w-2xl text-pretty text-lg font-light leading-relaxed text-neutral-300 sm:text-xl">
          Signed evidence reaches Base. The token stays on Solana.
        </p>

        <article className="prose-docs mt-12 max-w-2xl">
          <p>
            Selected Covenant registrations, schemas, and signed statements are
            readable on Base. $CVNT stays a single Solana mint.{" "}
            <code>ecrecover</code> can prove that a configured address signed the
            bytes; it does not establish the publisher&apos;s identity or prove the
            underlying score or claim.
          </p>

          <h2>Going multi-chain usually means moving the token</h2>
          <p>
            Crypto has lost billions to bridge hacks, and almost every one traces to the same
            decision. A project expands to a new chain by bridging or wrapping its token, and the
            wrapper becomes the honeypot. The value sits on the bridge, and the bridge is what gets
            drained.
          </p>
          <p>
            Signed evidence does not require moving the protocol token. A
            signature can bind bytes to a configured key without bridging
            value, but it cannot identify the publisher or turn a claim into a
            fact.
          </p>

          <h2>Project signed evidence, not the token</h2>
          <p>
            Covenant treats configured Solana identity and audit-root records as
            canonical references for this projection. Base holds registrations
            and signed statements, never the token. Consumers verify a
            configured signing key and then apply their own policy to the claim.
            Key attribution is a separate trust decision. Per-call
            value stays chain-local: USDC on the chain of the call, never $CVNT.
          </p>

          <h2>An issuer key an EVM can verify</h2>
          <p>
            Verifying a Solana signature on an EVM costs about 2 million gas,
            enough to make cross-chain verification pointless. So Covenant gives
            each identity a second key on the curve Ethereum already speaks,
            which an EVM recovers with a plain ecrecover at around 3 thousand
            gas. Covenant records associate that issuer with the configured
            Solana identifier. Recovering the configured key proves only that
            the corresponding private key signed the bytes, not who controls it
            or whether the statement is true.
          </p>

          <h2>What is live on Base today</h2>
          <p>
            The agent is registered in the ERC-8004 Identity Registry, so EVM
            tooling discovers a Covenant registration whose record points to a
            Solana address. The issuer record, EAS score schema, and
            bond-receipt verifier are deployed. A Covenant audit-root statement
            recovers to the configured issuer key. Deployment and internal
            review do not establish the truth of a signed claim, and onchain
            score writes and funded bonds are not exercised.
          </p>
          <p>
            opencovenant.eth and the per-agent CCIP-Read names resolve to
            configured Solana addresses. These are pointers, not proof of who
            controls or operates an agent. Covenant operates a Base x402-v2
            seller that can charge callers per request in USDC using EIP-3009
            authorization. Reusable outbound primitives exist, but the
            production daemon does not currently make Base x402 payments.
          </p>

          <h2>$CVNT never leaves Solana</h2>
          <p>
            $CVNT is one mint, one market. It is never bridged, wrapped, or
            minted on any other chain, and no current per-call fee is
            denominated in it. Known mint literals and named bridge patterns are
            guarded in repository validation. That guard is a tripwire, not
            proof against every future integration.
          </p>

          <h2>What is next</h2>
          <p>
            Bonds slashable on Solana from an EVM-proven event, with an objective fault definition
            and a challenge window. Then more L2s: the same attestation stack ships on every OP-Stack
            chain, so each new one is nearly free. Base is first.
          </p>

          <p>
            A key check proves key possession, not publisher identity or claim
            truth. The token and its market stay on Solana; selected signed
            evidence can be consumed elsewhere. The architecture and Base mainnet
            address sheet, with how to check every claim yourself, are in the{" "}
            <a href="/docs/multichain">multi-chain signed-evidence</a> docs.
          </p>
        </article>

        <div className="mt-16 flex flex-wrap gap-x-6 gap-y-3 border-t border-neutral-800/80 pt-8">
          <a
            href="/docs/multichain"
            className="text-[12px] uppercase tracking-[0.2em] text-neutral-400 transition-colors hover:text-neutral-50"
          >
            Address sheet →
          </a>
          <a
            href="https://github.com/open-covenant/covenant"
            target="_blank"
            rel="noopener noreferrer"
            className="text-[12px] uppercase tracking-[0.2em] text-neutral-400 transition-colors hover:text-neutral-50"
          >
            Source →
          </a>
        </div>
      </div>

      <SiteFooter className="pb-8" />
    </main>
  );
}
