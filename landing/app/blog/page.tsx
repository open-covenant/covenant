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

type Post = { slug: string; date: string; title: string; dek: string };

const POSTS: Post[] = [
  {
    slug: "covenant-payai",
    date: "21 June 2026",
    title: "Proof of work on the payment rail",
    dek: "PayAI moves the money. Covenant proves the work. A trust layer over PayAI's x402 rail: signed work-receipts and reputation from real on-chain settlements.",
  },
];

export default function BlogIndex() {
  return (
    <main id="main-content" className="relative min-h-screen overflow-x-hidden bg-[#030303]">
      <SiteHeader />

      <div className="page-container">
        <h1 className="mb-10 text-[11px] uppercase tracking-[0.4em] text-neutral-400 sm:mb-14">
          Blog
        </h1>

        <h2 className="max-w-3xl text-balance text-[2.2rem] font-extralight uppercase leading-[1.1] tracking-[2px] text-white sm:text-[2.6rem]">
          Notes from the build
        </h2>

        <p className="mt-6 max-w-2xl text-pretty text-lg font-light leading-relaxed text-neutral-300 sm:text-xl">
          What we ship, how the primitives work, and the integrations we are wiring up.
        </p>

        <ul className="mt-16 sm:mt-24">
          {POSTS.map((p) => (
            <li
              key={p.slug}
              className="border-t border-neutral-800/80 py-8 first:border-t-0 first:pt-0"
            >
              <a href={`/blog/${p.slug}`} className="group block">
                <span className="font-mono text-[11px] uppercase tracking-[0.3em] text-neutral-500">
                  {p.date}
                </span>
                <h3 className="mt-3 text-[1.4rem] font-light leading-snug text-neutral-100 transition-colors group-hover:text-white sm:text-[1.6rem]">
                  {p.title}
                </h3>
                <p className="mt-3 max-w-2xl text-[14px] leading-relaxed text-neutral-400 sm:text-[15px]">
                  {p.dek}
                </p>
                <span className="mt-4 inline-block text-[12px] uppercase tracking-[0.2em] text-neutral-500 transition-colors group-hover:text-neutral-200">
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
