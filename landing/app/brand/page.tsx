import type { Metadata } from "next";
import Image from "next/image";
import { SiteHeader } from "../SiteHeader";
import { SiteFooter } from "../SiteFooter";
import { ColorSwatch } from "./ColorSwatch";
import { RELEASE_STATUS, TAGLINE } from "../_brand";

const DESCRIPTION =
  "Covenant brand kit: logo, logomark, and wordmark downloads, the color palette, typography, and rules for using the name and marks.";

export const metadata: Metadata = {
  title: "Brand: Covenant",
  description: DESCRIPTION,
  alternates: { canonical: "/brand" },
  openGraph: {
    type: "website",
    url: "https://opencovenant.org/brand",
    title: "Covenant — brand & press kit",
    description: DESCRIPTION,
    images: [{ url: "/opengraph-image.jpg", width: 1200, height: 630 }],
  },
  twitter: {
    card: "summary_large_image",
    site: "@OpenCovenant",
    creator: "@OpenCovenant",
    title: "Covenant — brand & press kit",
    description: DESCRIPTION,
    images: "/twitter-image.jpg",
  },
};

const eyebrow = "font-mono text-[11px] uppercase tracking-[0.3em] text-neutral-400";
const sectionTitle = "text-[13px] uppercase tracking-[0.25em] text-neutral-100 sm:text-[14px]";
const paragraph = "text-[13px] leading-relaxed text-neutral-300 sm:text-[14px]";

const PALETTE: { name: string; hex: string; role: string; border?: boolean }[] = [
  { name: "Ink", hex: "#030303", role: "Background", border: true },
  { name: "Surface", hex: "#0a0a0a", role: "Cards", border: true },
  { name: "Neutral 900", hex: "#171717", role: "Raised" },
  { name: "Neutral 800", hex: "#262626", role: "Borders" },
  { name: "Neutral 500", hex: "#737373", role: "Muted" },
  { name: "Muted", hex: "#8a8a8a", role: "Secondary text" },
  { name: "Accent", hex: "#d4d4d4", role: "Emphasis" },
  { name: "Paper", hex: "#f5f5f5", role: "Foreground" },
];

const SANS_STACK = 'ui-sans-serif, system-ui, -apple-system, "Segoe UI", Roboto, sans-serif';
const MONO_STACK = 'ui-monospace, "SF Mono", Menlo, Consolas, monospace';

type Asset = { title: string; note: string; href: string; file: string; kind: string };

const ASSETS: Asset[] = [
  {
    title: "Lockup",
    note: "Primary lockup — wordmark and tagline together. Use this by default.",
    href: "/logo.svg",
    file: "covenant-logo.svg",
    kind: "SVG",
  },
  {
    title: "Wordmark",
    note: "The name on its own, set in the brand form, without the mark.",
    href: "/wordmark.png",
    file: "wordmark.png",
    kind: "PNG",
  },
  {
    title: "Logomark",
    note: "The </> mark on its own. For avatars, favicons, and tight spaces.",
    href: "/covenant-logomark.png",
    file: "covenant-logomark.png",
    kind: "PNG",
  },
];

function AssetCard({ asset }: { asset: Asset }) {
  const isMark = asset.file === "covenant-logomark.png";
  return (
    <div className="flex flex-col overflow-hidden rounded-lg border border-neutral-800/80 bg-[#0a0a0a]/60">
      <div className="flex h-40 items-center justify-center border-b border-neutral-800/70 bg-[#050505] px-8">
        <Image
          src={asset.href}
          alt={`Covenant ${asset.title.toLowerCase()}`}
          width={isMark ? 96 : 260}
          height={isMark ? 96 : 56}
          className={isMark ? "h-20 w-auto opacity-95" : "h-auto w-full max-w-[240px] opacity-95"}
        />
      </div>
      <div className="flex flex-1 flex-col gap-3 p-4">
        <div className="flex items-center justify-between gap-3">
          <span className="text-[13px] font-light tracking-wide text-neutral-100">{asset.title}</span>
          <span className="font-mono text-[10px] uppercase tracking-[0.2em] text-neutral-500">{asset.kind}</span>
        </div>
        <p className="text-[12px] leading-relaxed text-neutral-400">{asset.note}</p>
        <a
          href={asset.href}
          download={asset.file}
          className="mt-auto inline-flex w-fit items-center gap-2 rounded-md border border-neutral-700 px-3 py-1.5 text-[11px] uppercase tracking-[0.2em] text-neutral-300 transition-colors hover:border-neutral-500 hover:text-neutral-50"
        >
          Download ↓
        </a>
      </div>
    </div>
  );
}

function Section({
  label,
  title,
  children,
}: {
  label: string;
  title: string;
  children: React.ReactNode;
}) {
  return (
    <section className="border-t border-neutral-900 pt-10">
      <div className="mb-6 flex items-baseline gap-4">
        <span className="font-mono text-[11px] text-neutral-600">{label}</span>
        <h2 className={sectionTitle}>{title}</h2>
      </div>
      {children}
    </section>
  );
}

export default function BrandPage() {
  return (
    <main id="main-content" className="relative min-h-screen overflow-x-hidden bg-[#030303]">
      <SiteHeader />

      <div className="page-container">
        <div className="mx-auto w-full max-w-3xl">
          <p className={eyebrow}>Brand</p>
          <h1 className="mt-4 text-2xl font-extralight tracking-[0.12em] text-neutral-50 sm:text-3xl">
            Brand &amp; press kit
          </h1>
          <p className={`${paragraph} mt-4 max-w-xl`}>
            Everything you need to represent Covenant correctly — logos, colors, type, and the rules
            for using the name. Free to use in articles, integrations, and partner materials. If in
            doubt, keep it black and white and leave it room to breathe.
          </p>

          <div className="mt-14 flex flex-col gap-14">
            <Section label="01" title="Logo">
              <div className="grid grid-cols-1 gap-4 sm:grid-cols-3">
                {ASSETS.map((a) => (
                  <AssetCard key={a.file} asset={a} />
                ))}
              </div>
              <p className="mt-5 text-[12px] leading-relaxed text-neutral-500">
                The lockup and <span className="font-mono text-neutral-400">{"</>"}</span> mark are drawn
                in white for dark surfaces — the way Covenant is meant to be seen. Place them on {" "}
                <span className="font-mono text-neutral-400">#030303</span> through{" "}
                <span className="font-mono text-neutral-400">#171717</span>. For a light background,
                ask us for a dark variant rather than recoloring these.
              </p>
            </Section>

            <Section label="02" title="Clear space & sizing">
              <ul className="flex flex-col gap-3">
                {[
                  "Leave clear space around the logo at least equal to the height of the mark.",
                  "Minimum wordmark width is 120px on screen; minimum mark size is 24px.",
                  "Scale proportionally — never resize width and height independently.",
                ].map((t) => (
                  <li key={t} className="flex gap-3 text-[13px] leading-relaxed text-neutral-300">
                    <span aria-hidden className="mt-2 h-px w-4 shrink-0 bg-neutral-700" />
                    {t}
                  </li>
                ))}
              </ul>
            </Section>

            <Section label="03" title="Don't">
              <ul className="grid grid-cols-1 gap-3 sm:grid-cols-2">
                {[
                  "Recolor the marks or add gradients, glows, or shadows.",
                  "Stretch, rotate, skew, or otherwise distort them.",
                  "Set the logo on busy or low-contrast backgrounds.",
                  "Rebuild the wordmark in another typeface.",
                ].map((t) => (
                  <li
                    key={t}
                    className="flex gap-3 rounded-lg border border-neutral-800/70 bg-[#0a0a0a]/40 p-3.5 text-[12px] leading-relaxed text-neutral-300"
                  >
                    <span aria-hidden className="text-neutral-600">✕</span>
                    {t}
                  </li>
                ))}
              </ul>
            </Section>

            <Section label="04" title="Color">
              <p className={`${paragraph} mb-6 max-w-xl`}>
                Covenant is grayscale by design: near-black to near-white, no brand hue. Click any
                swatch to copy its hex.
              </p>
              <div className="grid grid-cols-1 gap-3 sm:grid-cols-2">
                {PALETTE.map((c) => (
                  <ColorSwatch key={c.hex} name={c.name} hex={c.hex} role={c.role} border={c.border} />
                ))}
              </div>
            </Section>

            <Section label="05" title="Typography">
              <div className="flex flex-col gap-5">
                <div className="rounded-lg border border-neutral-800/80 bg-[#0a0a0a]/60 p-5">
                  <p className="font-mono text-[10px] uppercase tracking-[0.25em] text-neutral-500">
                    Sans — interface & body
                  </p>
                  <p
                    className="mt-4 text-2xl font-extralight uppercase tracking-[0.14em] text-neutral-50"
                    style={{ fontFamily: SANS_STACK }}
                  >
                    Permission, not trust
                  </p>
                  <p
                    className="mt-3 text-[13px] leading-relaxed text-neutral-300"
                    style={{ fontFamily: SANS_STACK }}
                  >
                    Body copy is set light and generously spaced. Headings render uppercase with wide
                    tracking and an extralight weight. We use the system UI sans so the site feels
                    native on every platform and loads nothing extra.
                  </p>
                  <p className="mt-4 break-all font-mono text-[11px] text-neutral-500">{SANS_STACK}</p>
                </div>

                <div className="rounded-lg border border-neutral-800/80 bg-[#0a0a0a]/60 p-5">
                  <p className="font-mono text-[10px] uppercase tracking-[0.25em] text-neutral-500">
                    Mono — labels, data & code
                  </p>
                  <p className="mt-4 font-mono text-[15px] tracking-wide text-neutral-100">
                    intent · runtime · settlement
                  </p>
                  <p className="mt-3 font-mono text-[13px] leading-relaxed text-neutral-400">
                    Monospace carries eyebrows, stats, addresses, and anything machine-readable. It
                    signals precision without a custom typeface.
                  </p>
                  <p className="mt-4 break-all font-mono text-[11px] text-neutral-500">{MONO_STACK}</p>
                </div>
              </div>
            </Section>

            <Section label="06" title="Name & voice">
              <dl className="flex flex-col divide-y divide-neutral-900 border-y border-neutral-900">
                {[
                  { k: "Name", v: <>Always <span className="text-neutral-100">Covenant</span>, capital C, in running text. The lowercase wordmark is the only exception.</> },
                  { k: "Token", v: <>The token ticker is <span className="font-mono text-neutral-100">$CVNT</span>, uppercase, with the dollar sign.</> },
                  { k: "Tagline", v: <span className="text-neutral-100">{TAGLINE}.</span> },
                  { k: "Mark", v: <>The <span className="font-mono text-neutral-100">{"</>"}</span> logomark stands in for the whole system: <span className="text-neutral-100">human intent</span>, then <span className="text-neutral-100">agent protocol</span>, with code as the medium between them.</> },
                  { k: "Status", v: <><span className="font-mono uppercase tracking-[0.2em] text-neutral-100">{RELEASE_STATUS}</span> — the protocol is live and evolving in the open.</> },
                  { k: "One-liner", v: "Covenant is the operating layer for agentic software: every agent runs under a signed grant, and every action leaves a receipt." },
                ].map((row) => (
                  <div key={row.k} className="grid grid-cols-1 gap-1 py-4 sm:grid-cols-[128px_1fr] sm:gap-6">
                    <dt className="font-mono text-[11px] uppercase tracking-[0.2em] text-neutral-500">{row.k}</dt>
                    <dd className="text-[13px] leading-relaxed text-neutral-300">{row.v}</dd>
                  </div>
                ))}
              </dl>
            </Section>
          </div>
        </div>
      </div>

      <SiteFooter className="pb-8" />
    </main>
  );
}
