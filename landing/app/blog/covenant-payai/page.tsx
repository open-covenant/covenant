import type { Metadata } from "next";
import { SiteFooter } from "../../SiteFooter";
import { SiteHeader } from "../../SiteHeader";

export const metadata: Metadata = {
  title: "Proof of work on the payment rail",
  description:
    "PayAI moves the money. Covenant proves the work. A trust layer over PayAI's x402 rail: signed work-receipts and reputation built from real on-chain settlements, without ever touching payments.",
  alternates: { canonical: "/blog/covenant-payai" },
  openGraph: {
    type: "article",
    url: "https://opencovenant.org/blog/covenant-payai",
    title: "Proof of work on the payment rail",
    description:
      "PayAI moves the money. Covenant proves the work. Signed work-receipts and settlement-grounded reputation over PayAI's x402 rail.",
    images: [{ url: "/opengraph-image.jpg", width: 1200, height: 630 }],
  },
  twitter: {
    card: "summary_large_image",
    site: "@OpenCovenant",
    creator: "@OpenCovenant",
    title: "Proof of work on the payment rail",
    description: "PayAI moves the money. Covenant proves the work.",
    images: "/twitter-image.jpg",
  },
};

const backLink =
  "font-mono text-[12px] uppercase tracking-[0.2em] text-neutral-400 transition-colors hover:text-neutral-100";

export default function CovenantPayaiPost() {
  return (
    <main id="main-content" className="relative min-h-screen overflow-x-hidden bg-[#030303]">
      <SiteHeader />

      <div className="page-container">
        <a href="/blog" className={backLink}>
          ← Blog
        </a>

        <p className="mt-10 font-mono text-[11px] uppercase tracking-[0.3em] text-neutral-400">
          Integrations · 21 June 2026
        </p>

        <h1 className="mt-5 max-w-3xl text-balance text-[2.2rem] font-extralight uppercase leading-[1.1] tracking-[2px] text-white sm:text-[2.6rem]">
          Proof of work on the payment rail
        </h1>

        <p className="mt-6 max-w-2xl text-pretty text-lg font-light leading-relaxed text-neutral-300 sm:text-xl">
          PayAI moves the money. Covenant proves the work.
        </p>

        <article className="prose-docs mt-12 max-w-2xl">
          <h2>The gap in agent commerce</h2>
          <p>
            Agents are starting to pay each other. With x402, one agent calls another&apos;s
            endpoint, gets a 402, pays in stablecoin, and the work comes back. PayAI runs that rail:
            a hosted x402 facilitator that settles agent payments across chains, gasless, with no
            accounts and no API keys. It is clean and it works.
          </p>
          <p>
            But settlement is only half the deal. x402 moves the money and says nothing about
            delivery. The payment is instant and irreversible, which leaves the buyer with the one
            question the rail cannot answer: did the work actually happen? PayAI is upfront about
            this. Its own documentation notes that the protocol &ldquo;remains silent on delivery
            mechanisms, verification, or guarantees,&rdquo; and that the absence of chargebacks means
            you need robust delivery guarantees of your own.
          </p>
          <p>That gap is where Covenant lives.</p>

          <h2>What we built</h2>
          <p>
            Covenant is a trust layer that sits alongside the payment rail and never touches the
            money. It adds two things to a PayAI transaction:
          </p>
          <ul>
            <li>
              <strong>A signed work-receipt.</strong> After a settlement, the seller emits a receipt
              that binds the on-chain settlement to what was delivered: the resource, a hash of the
              output, and the seller&apos;s identity, signed with an ed25519 key. The buyer can
              counter-sign to accept. &ldquo;Took the money, did not deliver&rdquo; becomes provable
              instead of an argument.
            </li>
            <li>
              <strong>Settlement-grounded reputation.</strong> Covenant reads PayAI&apos;s public
              on-chain settlements and turns them into a signed reputation credential per agent: how
              many jobs it has actually settled, with how many distinct counterparties, and how much
              volume. It is a credential an agent can present, not a number on a page.
            </li>
          </ul>
          <p>
            Both are read-after-settlement and attest only. Covenant issues proofs, PayAI moves the
            money, and the two never overlap.
          </p>

          <h2>The results</h2>
          <p>
            This is built and proven end to end against PayAI&apos;s real on-chain settlements, not
            a mockup.
          </p>
          <p>
            The reputation oracle reads live settlements off chain. Pointed at an active seller on
            the rail, it produced a signed credential showing 114 settled jobs across 80 distinct
            counterparties, scored and verifiable on-chain. The work-receipt binds a real settlement
            transaction to the delivered output and checks against the signer&apos;s key. It runs as
            a Covenant daemon tool: an agent asks for a wallet&apos;s reputation and gets the signed
            credential back inline.
          </p>
          <p>
            The numbers are not seeded. They come straight from PayAI&apos;s settlement history, so
            an agent&apos;s reputation means something the moment it has done real work.
          </p>

          <h2>Why x402</h2>
          <p>
            x402 is the right rail to build trust on precisely because it is an open standard, not a
            closed platform. There is no integration to ask permission for and no gatekeeper in the
            middle. A trust layer can sit on top of the public settlement stream the same way anyone
            can read the chain. And the no-chargebacks property that makes x402 fast is exactly what
            makes delivery proof necessary. The rail is honest about what it does not do, which
            leaves a clean seam for what we do.
          </p>

          <h2>Why PayAI</h2>
          <p>
            PayAI owns agent payments and has deliberately stayed out of trust, delivery, and
            reputation. That is not a weakness to route around, it is the cleanest possible
            complement. They run the money rail, we run the proof rail, and neither touches the
            other. PayAI settles across chains with one integration and covers gas for both sides,
            which is what makes high-frequency, low-value agent commerce viable at all. Reputation
            built on that settlement volume is only meaningful because the rail underneath it is
            real.
          </p>
          <p>
            You can try the rail at{" "}
            <a href="https://payai.network" target="_blank" rel="noopener noreferrer">
              payai.network
            </a>{" "}
            and the facilitator at{" "}
            <a href="https://facilitator.payai.network" target="_blank" rel="noopener noreferrer">
              facilitator.payai.network
            </a>
            .
          </p>

          <h2>What is next</h2>
          <p>
            Receipts and reputation are the first two primitives. Next is wiring the reputation
            credential into spend and escrow decisions, so an agent can require a proven track record
            before it pays, and surfacing a Covenant Verified signal where buyers actually choose
            sellers. The trust layer is what makes autonomous, high-value agent commerce safe to run.
            The money already moves. Now the work is provable.
          </p>
        </article>

        <div className="mt-16 flex flex-wrap gap-x-6 gap-y-3 border-t border-neutral-800/80 pt-8">
          <a
            href="https://payai.network"
            target="_blank"
            rel="noopener noreferrer"
            className="text-[12px] uppercase tracking-[0.2em] text-neutral-400 transition-colors hover:text-neutral-50"
          >
            PayAI →
          </a>
          <a
            href="https://docs.opencovenant.org"
            target="_blank"
            rel="noopener noreferrer"
            className="text-[12px] uppercase tracking-[0.2em] text-neutral-400 transition-colors hover:text-neutral-50"
          >
            Documentation →
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
