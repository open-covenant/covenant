import type { Metadata } from "next";
import Image from "next/image";
import { SiteHeader } from "../SiteHeader";
import { SiteFooter } from "../SiteFooter";
import { CopyAddress } from "../CopyAddress";
import { RELEASE_STATUS, TAGLINE, X_URL, GITHUB_URL } from "../_brand";

const CVNT_MINT = "2mNVZ6aEjrGwiUVCfz7XGWpiXuWzgBDoznwE579upump";

const DESCRIPTION =
  "Every important Covenant link in one place: sandbox, docs, source, the $CVNT token, staking, the whitepaper, and where to reach us.";

export const metadata: Metadata = {
  title: "Links: Covenant",
  description: DESCRIPTION,
  alternates: { canonical: "/links" },
  openGraph: {
    type: "website",
    url: "https://opencovenant.org/links",
    title: "Covenant — all links",
    description: DESCRIPTION,
    images: [{ url: "/opengraph-image.jpg", width: 1200, height: 630 }],
  },
  twitter: {
    card: "summary_large_image",
    site: "@OpenCovenant",
    creator: "@OpenCovenant",
    title: "Covenant — all links",
    description: DESCRIPTION,
    images: "/twitter-image.jpg",
  },
};

type LinkItem = { label: string; href: string; note?: string };

const PRIMARY: LinkItem[] = [
  { label: "Try the sandbox", href: "https://sandbox.opencovenant.org", note: "Live operator console, no install" },
  { label: "Read the docs", href: "https://docs.opencovenant.org", note: "Concepts, primitives, API" },
  { label: "Source on GitHub", href: GITHUB_URL, note: "Apache-2.0, built in the open" },
];

const GROUPS: { title: string; items: LinkItem[] }[] = [
  {
    title: "$CVNT",
    items: [
      { label: "$CVNT", href: "/token", note: "Buy $CVNT, supply, and token details" },
      { label: "Stake $CVNT", href: "/stake", note: "Lock for a share of protocol revenue in SOL" },
      { label: "Treasury", href: "/treasury", note: "Read-only on-chain metrics" },
    ],
  },
  {
    title: "Learn",
    items: [
      { label: "Whitepaper", href: "https://doi.org/10.5281/zenodo.20134416", note: "The design, peer-citable" },
      { label: "Roadmap", href: "/roadmap" },
      { label: "About", href: "/about" },
      { label: "FAQ", href: "/faq" },
    ],
  },
  {
    title: "Connect",
    items: [
      { label: "X / @OpenCovenant", href: X_URL },
      { label: "Partners", href: "/partners" },
      { label: "Contact", href: "/contact" },
    ],
  },
];

function isExternal(href: string) {
  return href.startsWith("http");
}

function LinkButton({ item, primary = false }: { item: LinkItem; primary?: boolean }) {
  const external = isExternal(item.href);
  return (
    <a
      href={item.href}
      target={external ? "_blank" : undefined}
      rel={external ? "noopener noreferrer" : undefined}
      className={`group flex items-center justify-between gap-4 rounded-lg border px-5 transition-colors ${
        primary
          ? "border-neutral-700 bg-[#0d0d0d] py-4 hover:border-neutral-500 hover:bg-[#121212]"
          : "border-neutral-800/80 bg-[#0a0a0a]/60 py-3.5 hover:border-neutral-700 hover:bg-[#0f0f0f]"
      }`}
    >
      <span className="flex min-w-0 flex-col">
        <span className="truncate text-[13px] font-light tracking-wide text-neutral-100">
          {item.label}
        </span>
        {item.note ? (
          <span className="mt-0.5 truncate text-[11px] text-neutral-500">{item.note}</span>
        ) : null}
      </span>
      <span className="shrink-0 text-neutral-600 transition-colors group-hover:text-neutral-300">
        {external ? "↗" : "→"}
      </span>
    </a>
  );
}

export default function LinksPage() {
  return (
    <main id="main-content" className="relative min-h-screen overflow-x-hidden bg-[#030303]">
      <SiteHeader />

      <div className="page-container">
        <div className="mx-auto flex w-full max-w-md flex-col items-center">
          <Image src="/logomark.svg" alt="Covenant" width={52} height={52} priority className="opacity-90" />

          <h1 className="mt-6 text-[1.9rem] font-extralight uppercase leading-none tracking-[6px] text-white">
            Covenant
          </h1>

          <p className="mt-4 max-w-xs text-balance text-center text-[13px] font-light leading-relaxed text-neutral-400">
            {TAGLINE}
          </p>

          <span className="mt-5 rounded-full border border-neutral-800 px-3 py-1 text-[10px] uppercase tracking-[0.3em] text-neutral-400">
            {RELEASE_STATUS}
          </span>

          <div className="mt-10 flex w-full flex-col gap-2.5">
            {PRIMARY.map((item) => (
              <LinkButton key={item.href} item={item} primary />
            ))}
          </div>

          {GROUPS.map((group) => (
            <div key={group.title} className="mt-9 w-full">
              <h2 className="mb-3 text-[10px] uppercase tracking-[0.4em] text-neutral-500">
                {group.title}
              </h2>
              <div className="flex w-full flex-col gap-2.5">
                {group.items.map((item) => (
                  <LinkButton key={item.href + item.label} item={item} />
                ))}
              </div>
            </div>
          ))}

          <div className="mt-9 w-full">
            <h2 className="mb-3 text-[10px] uppercase tracking-[0.4em] text-neutral-500">
              $CVNT mint
            </h2>
            <div className="flex flex-col gap-2.5">
              <CopyAddress address={CVNT_MINT} label="Copy $CVNT mint address" />
              <a
                href={`https://solscan.io/token/${CVNT_MINT}`}
                target="_blank"
                rel="noopener noreferrer"
                className="text-[11px] uppercase tracking-[0.2em] text-neutral-500 transition-colors hover:text-neutral-200"
              >
                View on Solscan ↗
              </a>
            </div>
          </div>
        </div>
      </div>

      <SiteFooter className="pb-8" />
    </main>
  );
}
