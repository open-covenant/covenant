import type { Metadata } from "next";
import { pageMetadata } from "@/app/_metadata";
import Image from "next/image";
import Link from "next/link";
import type { ReactNode } from "react";
import { CovenantMark } from "../_brand";
import { ResolveDemo } from "./_demos";
import { Chip, MoreSections, PageHead, Panel, Part } from "./_kit";
import { BRAND_ASSETS_ZIP, BRAND_SECTIONS } from "./_sections";
import { PartnerLockup, StaticField, SubBrandLockup } from "./_visuals";

const DESCRIPTION =
  "The Covenant brand guidelines: messaging, the logo, color, typography, imagery, sub-brands, motion, partnerships and the identity in use, with every asset to download.";

export const metadata: Metadata = pageMetadata("/brand", { title: "Brand guidelines" }, {
  title: "Brand guidelines: Covenant",
  description: DESCRIPTION,
  alternates: { canonical: "/brand" },
  openGraph: {
    type: "website",
    url: "https://opencovenant.org/brand",
    title: "Covenant brand guidelines",
    description: DESCRIPTION,
  },
  twitter: {
    card: "summary_large_image",
    site: "@OpenCovenant",
    creator: "@OpenCovenant",
    title: "Covenant brand guidelines",
    description: DESCRIPTION,
  },
});

// One live tile per section, so the index shows the system rather than names.
const TILES: Record<string, ReactNode> = {
  messaging: (
    <p className="type-display max-w-[13ch] text-[clamp(1.25rem,2.2vw,1.65rem)] text-fg">
      A receipt for every decision<span className="term-cursor text-mint">_</span>
    </p>
  ),
  logo: <CovenantMark className="h-[46%] w-auto" />,
  color: (
    <span className="grid h-full w-full grid-cols-[2fr_1fr_1fr_1.4fr]" aria-hidden>
      <span className="flex items-end bg-ground p-3 font-mono text-[9px] uppercase tracking-[0.18em] text-muted-2">Background</span>
      <span className="bg-raised" />
      <span className="bg-line" />
      <span className="bg-ramp" />
    </span>
  ),
  typography: (
    <span className="flex flex-col items-center gap-3">
      <span className="flex items-baseline text-fg">
        <span className="type-display text-[clamp(3.2rem,6vw,4.6rem)]">A</span>
        <span className="font-mono text-[clamp(2.8rem,5.2vw,4rem)] font-medium leading-none">a</span>
      </span>
      <span className="font-mono text-[11px] tracking-[0.2em] text-muted">ARCHIVO · JETBRAINS MONO</span>
    </span>
  ),
  imagery: <StaticField cols={36} rows={27} time={3.1} className="h-full w-full" />,
  "sub-brands": <SubBrandLockup descriptor="Guard" fontSize={10} />,
  motion: (
    <span className="flex h-[42%] items-center justify-center" aria-hidden>
      <span className="brand-module block aspect-square h-full bg-ramp" />
    </span>
  ),
  partnerships: <PartnerLockup fontSize={7.5} />,
  "in-use": <Image src="/brand/in-use/og-card.jpg" alt="" width={1200} height={630} className="h-full w-full object-cover" />,
};

const PRINCIPLES = [
  {
    title: "Built as a system",
    body: "The logo, the imagery and the interface share one construction, so every touchpoint is recognizably Covenant.",
  },
  {
    title: "Permission, not trust",
    body: "Every element has a defined role and clear limits. The gradient has four uses, and no others.",
  },
  {
    title: "A receipt for every decision",
    body: "Claims come with evidence: the commit, the audit entry, the transaction.",
  },
  {
    title: "Built in the open",
    body: "We build in public, credit the work we rely on, and make every brand asset free to download.",
  },
  {
    title: "Yours to use",
    body: "Use the logo and files in articles, integrations and partner material without asking.",
  },
  {
    title: "Stated plainly",
    body: "Describe what exists today. No roadmap presented as fact, no superlatives.",
  },
];

const ASSET_TREE = [
  ["logo/", "mark, lockup, one-color white · SVG + PNG"],
  ["icons/", "app icon, avatar, favicon · PNG"],
  ["social/", "share card 1200 × 630, X header 1500 × 500"],
  ["color/", "color tokens as CSS and JSON, values as text"],
  ["README.txt", "the essentials on one page"],
];

export default function BrandIntroduction() {
  const sections = BRAND_SECTIONS.filter((s) => s.slug !== "introduction");
  return (
    <article>
      <PageHead slug="introduction" title="Brand guidelines">
        <Panel tone="ground">
          <ResolveDemo className="aspect-[16/10] w-full sm:aspect-[16/8]" />
        </Panel>
        <p className="mt-12 max-w-[46rem] border-t border-line pt-10 text-[16px] leading-[1.75] text-fg-soft">
          Covenant began with one idea: an agent should only do what it has been given permission to do. It
          has grown into an open operating layer whose own agents build it in public. These guidelines
          capture that shift. They are less a rulebook than a foundation: they set out the non-negotiables
          that keep Covenant recognizable, and the open space where builders, partners and the community
          are free to make it their own. Use them to stay consistent, never confined. If a choice makes
          Covenant clearer, more verifiable or more useful, make it, then share what you learned, so the
          guidelines keep evolving.
        </p>
      </PageHead>

      <Part id="sections" kicker="Contents" title="Sections" wide>
        <ul className="grid gap-x-4 gap-y-8 sm:grid-cols-2 xl:grid-cols-3">
          {sections.map((s) => (
            <li key={s.slug}>
              <Link href={s.href} className="group flex flex-col gap-3">
                <span className="flex aspect-[4/3] items-center justify-center overflow-hidden rounded-[6px] border border-line bg-raised transition-colors group-hover:border-line-strong">
                  {TILES[s.slug]}
                </span>
                <Chip>
                  <span className="mr-2 tabular-nums text-faint">{s.n}</span>
                  {s.title}
                </Chip>
                <span className="text-[12px] leading-relaxed text-muted transition-colors group-hover:text-fg-soft">{s.summary}</span>
              </Link>
            </li>
          ))}
        </ul>
      </Part>

      <Part
        id="principles"
        kicker="Foundations"
        title="Principles"
        wide
        text={
          <p>The brand follows the same principles as the software.</p>
        }
      >
        <ol className="grid border-t border-line sm:grid-cols-2 lg:grid-cols-3">
          {PRINCIPLES.map((p, i) => (
            <li
              key={p.title}
              className="flex flex-col gap-3 border-b border-line py-6 sm:pr-6 lg:[&:not(:nth-child(3n))]:border-r lg:[&:not(:nth-child(3n+1))]:pl-6"
            >
              <span className="font-mono text-[10px] tabular-nums tracking-[0.2em] text-faint">{String(i + 1).padStart(2, "0")}</span>
              <span className="font-mono text-[12px] font-semibold uppercase tracking-[0.16em] text-fg">{p.title}</span>
              <span className="text-[12.5px] leading-relaxed text-muted">{p.body}</span>
            </li>
          ))}
        </ol>
      </Part>

      <Part
        id="assets"
        kicker="Downloads"
        title="Brand assets"
        text={
          <>
            <p>
              The logo and lockup in SVG and PNG, the app icon and avatar, the share card and X header, and
              the color palette as design tokens.
            </p>
            <p>
              Free to use in articles, integrations and partner material. For anything else,{" "}
              <Link href="/contact" className="text-fg underline decoration-line-strong underline-offset-[3px] hover:decoration-blue">
                contact us
              </Link>
              .
            </p>
          </>
        }
      >
        <Panel tone="sunken">
          <div className="flex flex-col gap-6 p-6 sm:flex-row sm:items-center sm:justify-between">
            <ul className="flex flex-col gap-2 font-mono text-[11.5px]">
              {ASSET_TREE.map(([name, what]) => (
                <li key={name} className="flex flex-wrap gap-x-3">
                  <span className="w-24 text-fg">{name}</span>
                  <span className="text-muted">{what}</span>
                </li>
              ))}
            </ul>
            <a href={BRAND_ASSETS_ZIP} download className="btn btn-primary shrink-0 self-start sm:self-auto">
              Download all <span aria-hidden="true">↓</span>
            </a>
          </div>
        </Panel>
      </Part>

      <MoreSections slug="introduction" />
    </article>
  );
}
