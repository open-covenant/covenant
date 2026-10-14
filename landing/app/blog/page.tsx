import type { Metadata } from "next";
import { SiteFooter } from "../SiteFooter";
import { SiteHeader } from "../SiteHeader";

export const metadata: Metadata = {
  title: "Blog",
  description:
    "Notes from the Covenant build: integrations, the primitives behind the trust layer, and what actually ships.",
  alternates: { canonical: "/blog" },
  openGraph: {
    type: "website",
    url: "https://opencovenant.org/blog",
    title: "Covenant Blog",
    description: "Notes from the build: integrations, primitives, and what ships.",
    images: [{ url: "/opengraph-image.jpg", width: 1200, height: 630 }],
  },
  twitter: {
    card: "summary_large_image",
    site: "@OpenCovenant",
    creator: "@OpenCovenant",
    title: "Covenant Blog",
    description: "Notes from the build: integrations, primitives, and what ships.",
    images: "/twitter-image.jpg",
  },
};

type Post = { slug: string; date: string; title: string; dek: string; href?: string };

const POSTS: Post[] = [
  {
    slug: "compute-mainnet",
    href: "/compute",
    date: "29 September 2026",
    title: "Covenant Compute is live on Solana mainnet",
    dek: "Rent a GPU by the second and pay in USDC. The meter runs onchain in a MagicBlock ephemeral rollup, and every node stakes CVNT to take work.",
  },
  {
    slug: "compute",
    date: "12 August 2026",
    title: "The model you paid for is not always the model that ran",
    dek: "Every request on the inference network returns a signed receipt naming the model that served it, with hashes of the input and output. The prompt itself is never stored.",
  },
  {
    slug: "covenant-is-now-multichain",
    date: "7 July 2026",
    title: "Covenant is now multi-chain. The token isn't.",
    dek: "The trust layer is live on Base while $CVNT stays a single Solana mint. Only signed data crosses chains, verifiable with a plain ecrecover.",
  },
  {
    slug: "covenant-payai",
    date: "21 June 2026",
    title: "Proof of work on the payment rail",
    dek: "PayAI moves the money. Covenant proves the work. A trust layer over PayAI's x402 rail: signed work-receipts and reputation from real on-chain settlements.",
  },
];

export default function BlogIndex() {
  return (
    <main id="main-content" className="relative min-h-screen overflow-x-hidden bg-ground">
      <SiteHeader />

      <div className="page-container">
        <h1 className="mb-10 text-[11px] uppercase tracking-[0.4em] text-muted sm:mb-14">
          Blog
        </h1>

        <h2 className="type-display max-w-3xl text-balance text-[2.2rem] text-fg sm:text-[2.6rem]">
          Notes from the build
        </h2>

        <p className="mt-6 max-w-2xl text-pretty text-[15px] leading-[1.7] text-fg-soft sm:text-base">
          What we ship, how the primitives work, and the integrations we are wiring up.
        </p>

        <ul className="mt-16 sm:mt-24">
          {POSTS.map((p) => (
            <li
              key={p.slug}
              className="border-t border-line/80 py-8 first:border-t-0 first:pt-0"
            >
              <a href={p.href ?? `/blog/${p.slug}`} className="group block">
                <span className="font-mono text-[11px] uppercase tracking-[0.3em] text-muted-2">
                  {p.date}
                </span>
                <h3 className="type-display mt-3 text-[1.4rem] text-fg transition-colors group-hover:text-fg sm:text-[1.6rem]">
                  {p.title}
                </h3>
                <p className="mt-3 max-w-2xl text-[14px] leading-relaxed text-muted sm:text-[15px]">
                  {p.dek}
                </p>
                <span className="mt-4 inline-block text-[12px] uppercase tracking-[0.2em] text-muted-2 transition-colors group-hover:text-fg-2">
                  Read →
                </span>
              </a>
            </li>
          ))}
        </ul>
      </div>

      <SiteFooter className="pb-8" />
    </main>
  );
}
