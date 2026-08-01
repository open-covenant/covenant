import type { Metadata } from "next";
import { SiteFooter } from "../../SiteFooter";
import { SiteHeader } from "../../SiteHeader";

export const metadata: Metadata = {
  title: "Correction: what payment evidence proves",
  description:
    "A correction to Covenant's June 2026 PayAI experiment: a chain-confirmed transfer proves funds moved, not delivery, quality, or reputation.",
  alternates: { canonical: "/blog/covenant-payai" },
  openGraph: {
    type: "article",
    url: "https://opencovenant.org/blog/covenant-payai",
    title: "Correction: what payment evidence proves",
    description:
      "A chain-confirmed transfer proves funds moved. A seller-signed receipt proves the seller signed a statement. Neither proves delivery or quality.",
    images: [{ url: "/opengraph-image.jpg", width: 1200, height: 630 }],
  },
  twitter: {
    card: "summary_large_image",
    site: "@OpenCovenant",
    creator: "@OpenCovenant",
    title: "Correction: what payment evidence proves",
    description:
      "Chain-confirmed transfers and signed statements are evidence, not proof of work or reputation.",
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
          Correction · 31 July 2026
        </p>

        <h1 className="mt-5 max-w-3xl text-balance text-[2.2rem] font-extralight uppercase leading-[1.1] tracking-[2px] text-white sm:text-[2.6rem]">
          Correction: what payment evidence proves
        </h1>

        <p className="mt-6 max-w-2xl text-pretty text-lg font-light leading-relaxed text-neutral-300 sm:text-xl">
          Chain-confirmed transfers and signed statements are evidence. They are
          not proof of delivery or quality.
        </p>

        <article className="prose-docs mt-12 max-w-2xl">
          <h2>What the original post got wrong</h2>
          <p>
            The June 2026 version of this post inferred &ldquo;jobs&rdquo; and
            &ldquo;reputation&rdquo; from public payment activity. That
            inference was too strong. A chain-confirmed transfer proves that
            funds moved. It does not prove why they moved, whether an x402 job
            existed, or whether delivery was correct.
          </p>
          <p>
            A seller-signed receipt proves that the seller signed a particular
            statement. It can bind a payment identifier, resource, or output
            digest to that statement, but it is not independent proof that the
            service was delivered or that the output was useful. A signature
            proves possession of the corresponding key and detects changed
            bytes. Publisher attribution requires an expected key pinned through
            a trusted external channel, and the signature does not make the
            statement true.
          </p>
          <h2>What the experiment actually demonstrated</h2>
          <p>
            Covenant built two experimental evidence formats alongside the
            payment rail:
          </p>
          <ul>
            <li>
              <strong>A seller-signed statement.</strong> The seller can sign a
              canonical payload that references a reported transfer and an
              output digest. A verifier with an independently pinned expected
              key can detect payload changes and attribute the signature to that
              key.
            </li>
            <li>
              <strong>Bounded transfer observations.</strong> Covenant can
              report transfers associated with configured PayAI-linked
              addresses, together with coverage and provenance labels. Those
              observations are not jobs or reputation.
            </li>
          </ul>
          <p>
            The format and signature checks ran end to end. That validates the
            software path, not the commercial meaning originally assigned to the
            data.
          </p>
          <h2>What changes now</h2>
          <p>
            Covenant is retiring the public reputation interpretation and
            keeping the underlying artifacts only as experimental payment
            evidence. Public descriptions now distinguish observed transfers,
            publisher-key-signed statements, and independently established
            delivery evidence.
          </p>
          <p>
            Any future policy decision must consume typed, provenance-labelled
            evidence and state what each input can establish. A settlement can
            support accounting. A signed offer can bind commercial terms. A
            signed receipt can attribute a delivery assertion. Buyer acceptance
            or an independent verifier is still required for a delivery or
            quality claim.
          </p>
          <h2>Why retain the evidence</h2>
          <p>
            x402 remains useful because its payment artifacts can be linked to
            offers, identifiers, settlement results, and service receipts. The
            honest product boundary is to preserve and verify those artifacts
            without converting them into an unsupported trust score.
          </p>
          <p>
            Learn more about PayAI at{" "}
            <a
              href="https://payai.network"
              target="_blank"
              rel="noopener noreferrer"
            >
              payai.network
            </a>{" "}
            and the facilitator at{" "}
            <a href="https://facilitator.payai.network" target="_blank" rel="noopener noreferrer">
              facilitator.payai.network
            </a>
            .
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
