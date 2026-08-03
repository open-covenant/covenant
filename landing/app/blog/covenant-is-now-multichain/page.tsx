import type { Metadata } from "next";
import { SiteFooter } from "../../SiteFooter";
import { SiteHeader } from "../../SiteHeader";

export const metadata: Metadata = {
  title: "Covenant is now multi-chain. The token isn't.",
  description:
    "Covenant's trust layer is live on Base while $CVNT stays a single Solana mint. Only signed data crosses chains, verifiable with a plain ecrecover.",
  alternates: { canonical: "/blog/covenant-is-now-multichain" },
  openGraph: {
    type: "article",
    url: "https://opencovenant.org/blog/covenant-is-now-multichain",
    title: "Covenant is now multi-chain. The token isn't.",
    description:
      "The trust layer is live on Base while $CVNT stays a single Solana mint. Only signed data crosses chains, verifiable with a plain ecrecover.",
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
    description: "Trust crosses to Base. The token stays on Solana.",
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
          Trust crosses to Base. The token stays on Solana.
        </p>

        <article className="prose-docs mt-12 max-w-2xl">
          <p>
            Covenant&apos;s trust layer is live on Base. $CVNT stays a single Solana mint. What
            crosses to Base is proof, not value: signed statements about an agent that any EVM
            contract can check for a fraction of a cent.
          </p>

          <h2>Going multi-chain usually means moving the token</h2>
          <p>
            Crypto has lost billions to bridge hacks, and almost every one traces to the same
            decision. A project expands to a new chain by bridging or wrapping its token, and the
            wrapper becomes the honeypot. The value sits on the bridge, and the bridge is what gets
            drained.
          </p>
          <p>
            An agent trust layer does not need to move value across chains. It needs to move facts:
            who an agent is, what it has done, what it has staked. Facts are signatures, and
            signatures do not need a bridge.
          </p>

          <h2>Bridge the trust, not the token</h2>
          <p>
            Covenant keeps one canonical identity and one hash-chained audit history authoritative
            on Solana. Base holds verifiable projections of them, never the source of truth and never
            the token. An agent&apos;s identity, reputation, provenance, and bonds each cross as a
            signed statement a contract verifies on arrival. Only signed data crosses, never a
            wrapped token and never custody. Per-call value stays chain-local: USDC on the chain of
            the call, never $CVNT.
          </p>

          <h2>One identity an EVM can check for ~3k gas</h2>
          <p>
            Verifying a Solana signature on an EVM costs about 2 million gas, enough to make
            cross-chain verification pointless. So Covenant gives each identity a second key on the
            curve Ethereum already speaks, which an EVM recovers with a plain ecrecover at around 3
            thousand gas, and binds it bidirectionally to the canonical Solana identity. Every
            cross-chain record is signed by that key. A consumer recovers the signer with one call
            and checks it against Covenant&apos;s published address. One agent, one record, provable
            on both chains, for a fraction of a cent.
          </p>

          <h2>What is live on Base today</h2>
          <p>
            The agent is registered in the ERC-8004 Identity Registry, so EVM tooling discovers a
            Covenant identity whose record points back to Solana. The issuer identity, a
            non-transferable reputation schema bound to a Solana anchor, and a bond-receipt verifier
            are all deployed. A real Covenant provenance record already verifies under Base&apos;s own
            attestation stack and recovers to the issuer key. The entire surface went through an
            internal adversarial security audit and hardening before any of it shipped.
          </p>
          <p>
            opencovenant.eth resolves to the Covenant identity, and per-agent names resolve to each
            agent&apos;s Solana identity through a CCIP-Read gateway. Agents pay and charge per call
            in USDC on Base over x402, gasless through an EIP-3009 authorization so an agent needs
            only USDC.
          </p>

          <h2>$CVNT never leaves Solana</h2>
          <p>
            $CVNT is one mint, one market. It is never bridged, wrapped, or minted on any other
            chain, and no per-call fee is ever denominated in it. This is enforced in code and
            checked on every build. Multi-chain grows the surface that consumes Covenant&apos;s trust
            without ever fragmenting the token or the trust root.
          </p>

          <h2>What is next</h2>
          <p>
            Bonds slashable on Solana from an EVM-proven event, with an objective fault definition
            and a challenge window. Then more L2s: the same attestation stack ships on every OP-Stack
            chain, so each new one is nearly free. Base is first.
          </p>

          <p>
            Not trust by claim. Trust checked against the key. The token and its market stay whole on
            Solana; the proof travels everywhere else. The full architecture and the Base mainnet
            address sheet, with how to check every claim yourself, are in the{" "}
            <a href="/docs/multichain">multi-chain trust</a> docs.
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
            href="https://github.com/open-covenant"
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
