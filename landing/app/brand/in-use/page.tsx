import type { Metadata } from "next";
import { pageMetadata } from "@/app/_metadata";
import Image from "next/image";
import Link from "next/link";
import { Menu } from "lucide-react";
import { CovenantMark, RELEASE_STATUS, TAGLINE } from "../../_brand";
import { MoreSections, PageHead, Panel, Part } from "../_kit";
import { Lockup, StaticField } from "../_visuals";

const DESCRIPTION =
  "The Covenant identity at work: the website, social cards, profiles, documentation, print, slides and a poster.";

export const metadata: Metadata = pageMetadata("/brand/in-use", { title: "In use", section: "Brand guidelines" }, {
  title: "In use: Covenant brand guidelines",
  description: DESCRIPTION,
  alternates: { canonical: "/brand/in-use" },
  openGraph: {
    type: "website",
    url: "https://opencovenant.org/brand/in-use",
    title: "In use · Covenant brand guidelines",
    description: DESCRIPTION,
  },
});

function Browser({ url, children }: { url: string; children: React.ReactNode }) {
  return (
    <div className="overflow-hidden rounded-[8px] border border-line bg-sunken">
      <div className="flex items-center gap-3 border-b border-line px-3 py-2.5">
        <span className="flex gap-1.5" aria-hidden>
          <span className="h-2.5 w-2.5 rounded-full bg-line-strong" />
          <span className="h-2.5 w-2.5 rounded-full bg-line-strong" />
          <span className="h-2.5 w-2.5 rounded-full bg-line-strong" />
        </span>
        <span className="flex-1 truncate rounded-full bg-ground px-3 py-1 text-center font-mono text-[10.5px] text-muted">{url}</span>
      </div>
      {children}
    </div>
  );
}

/** A 16:9 slide, sized by its container. */
function Slide({ children, className = "" }: { children: React.ReactNode; className?: string }) {
  return <div className={`relative aspect-[16/9] w-full overflow-hidden rounded-[6px] border border-line bg-ground [container-type:inline-size] ${className}`}>{children}</div>;
}

export default function BrandInUse() {
  return (
    <article>
      <PageHead
        slug="in-use"
        intro={
          <p>
            Every Covenant touchpoint is built from the same elements: the dark background, the field, the
            Covenant gradient, two typefaces and the mark.
          </p>
        }
      >
        <Panel tone="sunken">
          <div className="p-4 sm:p-8">
            <Browser url="opencovenant.org">
              <Image src="/brand/in-use/site.jpg" alt="The Covenant home page" width={1440} height={900} className="h-auto w-full" priority />
            </Browser>
          </div>
        </Panel>
      </PageHead>

      <Part
        id="website"
        kicker="In use"
        title="Website"
        text={
          <>
            <p>
              On the home page, the opening animation clears the dark screen to reveal the page and leaves the
              field drifting behind the headline. As you scroll, the field comes together to form the mark. A band of the field
              separates each section from the next.
            </p>
            <p>
              <Link href="/" className="text-fg underline decoration-line-strong underline-offset-[3px] hover:decoration-blue">
                opencovenant.org
              </Link>
            </p>
          </>
        }
      >
        <div className="grid gap-4 sm:grid-cols-2">
          <Panel tone="ground" label="Header">
            <div className="flex h-28 items-center justify-between px-5">
              <Lockup fontSize={9} />
              <Menu aria-hidden className="h-4 w-4 text-fg" />
            </div>
          </Panel>
          <Panel tone="ground" label="Section band">
            <div className="relative h-28">
              <StaticField strip cols={64} rows={10} time={1.7} className="absolute inset-0 h-full w-full" />
            </div>
          </Panel>
        </div>
      </Part>

      <Part
        id="social"
        kicker="In use"
        title="Social cards"
        wide
        text={
          <p>
            The share card uses the same field as the home page and the site&apos;s typography. Release
            notes and integration cards share its layout: the dark background, a band of the field and a
            single headline.
          </p>
        }
      >
        <div className="grid gap-4 lg:grid-cols-2">
          <Panel tone="ground" label="Share card · 1200 × 630">
            <Image src="/brand/in-use/og-card.jpg" alt="The Covenant share card" width={1200} height={630} className="h-auto w-full" />
          </Panel>
          <Panel tone="ground" label="Release note · 1200 × 630">
            <div className="relative flex aspect-[1200/630] flex-col justify-between overflow-hidden p-[6%] [container-type:inline-size]">
              <div className="relative flex items-center justify-between">
                <Lockup fontSize={8} />
                <span className="font-mono text-[2.2cqw] uppercase tracking-[0.2em] text-mint">{RELEASE_STATUS}</span>
              </div>
              <StaticField strip cols={80} rows={10} time={3.6} className="absolute inset-x-0 top-[36%] h-[26%] w-full" />
              <div className="relative">
                <p className="font-mono text-[2cqw] uppercase tracking-[0.24em] text-muted">v0.1.0-alpha.1</p>
                <p className="type-display mt-[2cqw] text-[6.4cqw] text-fg">Alpha release</p>
              </div>
            </div>
          </Panel>
        </div>
      </Part>

      <Part
        id="profile"
        kicker="In use"
        title="Profiles"
        text={
          <p>
            Avatar: the mark on the Covenant background at half the circle&apos;s height. Header: a wide band
            of the field, with its left third kept clear for the avatar.
          </p>
        }
      >
        <Panel tone="ground" label="X profile">
          <div>
            <Image src="/brand/in-use/x-header.jpg" alt="The Covenant X header" width={1500} height={500} className="h-auto w-full" />
            <div className="relative px-5 pb-6">
              <span className="-mt-10 flex h-20 w-20 items-center justify-center rounded-full border-4 border-ground bg-ground sm:-mt-12 sm:h-24 sm:w-24">
                <span className="flex h-1/2">
                  <CovenantMark className="h-full w-auto" />
                </span>
              </span>
              <p className="mt-3 text-[15px] font-semibold text-fg">Covenant</p>
              <p className="text-[12px] text-muted-2">@OpenCovenant</p>
              <p className="mt-3 text-[12.5px] leading-relaxed text-fg-soft">{TAGLINE}.</p>
            </div>
          </div>
        </Panel>
      </Part>

      <Part
        id="docs"
        kicker="In use"
        title="Documentation"
        text={
          <p>
            The documentation uses the same system, set for long reading: the lockup at the top of the
            sidebar, display titles, body text in JetBrains Mono at 13.5 px and code on Inset.
          </p>
        }
      >
        <Browser url="docs.opencovenant.org">
          <Image src="/brand/in-use/docs.jpg" alt="The Covenant documentation" width={1440} height={900} className="h-auto w-full" />
        </Browser>
      </Part>

      <Part
        id="print"
        kicker="In use"
        title="Print"
        text={
          <p>
            Print is full bleed on the dark background, with no white margins. Print the background color, and
            proof the gradient on dark stock. Documents follow the screen layout: lockup, label, headline and
            body text.
          </p>
        }
      >
        <Panel tone="sunken">
          <div className="flex justify-center p-6 sm:p-10">
            <div className="relative aspect-[1/1.414] w-full max-w-[22rem] overflow-hidden rounded-[4px] border border-line bg-ground shadow-[0_24px_60px_rgb(0_0_0/0.5)] [container-type:inline-size]">
              {/* Padding lives on this inner box: cqw on the container itself would resolve against the viewport. */}
              <div className="flex h-full flex-col p-[9cqw]">
                <div className="flex items-center justify-between border-b border-line pb-[4cqw]">
                  <Lockup fontSize={7} />
                  <span className="font-mono text-[2.3cqw] uppercase tracking-[0.2em] text-muted-2">opencovenant.org</span>
                </div>
                <p className="mt-[8cqw] font-mono text-[2.4cqw] uppercase tracking-[0.26em] text-muted">Brief</p>
                <p className="type-display mt-[3cqw] text-[8.4cqw] text-fg">Permission, not trust</p>
                <p className="mt-[5cqw] text-[2.9cqw] leading-[1.7] text-fg-soft">
                  Covenant is the open, local-first operating layer for agentic software. It gives humans and
                  agents eight host-level primitives so they can safely share one computer.
                </p>
                <ul className="mt-[5cqw] flex flex-col gap-[2.4cqw] border-t border-line pt-[5cqw]">
                  {["Every agent runs under a signed grant", "Every action leaves a receipt", "Built in the open"].map((t) => (
                    <li key={t} className="flex items-center gap-[2.6cqw] font-mono text-[2.6cqw] text-fg-soft">
                      <span className="h-[1.6cqw] w-[1.6cqw] shrink-0 rounded-[0.4cqw] bg-line-strong" />
                      {t}
                    </li>
                  ))}
                </ul>
                <p className="mt-auto font-mono text-[2.2cqw] uppercase tracking-[0.2em] text-faint">Open source · Apache-2.0</p>
              </div>
            </div>
          </div>
        </Panel>
      </Part>

      <Part
        id="slides"
        kicker="In use"
        title="Slides"
        wide
        text={
          <p>
            Three slide layouts cover most presentations: a title, a key point and a close. Use the dark
            background, one idea per slide, and the field on the title slide only.
          </p>
        }
      >
        <div className="grid gap-4 lg:grid-cols-3">
          <Slide>
            <StaticField cols={48} rows={27} time={2.4} clearLeft className="absolute inset-0 h-full w-full opacity-90" />
            <div className="relative flex h-full flex-col justify-between p-[6cqw]">
              <span className="font-mono text-[2.6cqw] uppercase tracking-[0.24em] text-muted">Covenant</span>
              <p className="type-display max-w-[11ch] text-[8.2cqw] text-fg">An operating system that builds itself</p>
            </div>
          </Slide>
          <Slide>
            <div className="flex h-full flex-col justify-between p-[6cqw]">
              <span className="font-mono text-[2.4cqw] uppercase tracking-[0.24em] text-muted">02 · Permission, not trust</span>
              <p className="type-display text-[7cqw] text-fg">
                Every action leaves a <span className="text-ramp">receipt</span>
              </p>
              <div className="grid grid-cols-3 gap-[2cqw] border-t border-line pt-[3cqw]">
                {["Intent", "Grant", "Receipt"].map((t, i) => (
                  <span key={t} className="font-mono text-[2.3cqw] uppercase tracking-[0.18em] text-fg-soft">
                    <span className="text-faint">0{i + 1} </span>
                    {t}
                  </span>
                ))}
              </div>
            </div>
          </Slide>
          <Slide>
            <div className="flex h-full flex-col items-center justify-center gap-[4cqw]">
              <span className="flex h-[30%]">
                <CovenantMark className="h-full w-auto" />
              </span>
              <span className="font-mono text-[2.8cqw] uppercase tracking-[0.24em] text-fg">opencovenant.org</span>
            </div>
          </Slide>
        </div>
      </Part>

      <Part
        id="poster"
        kicker="In use"
        title="Poster"
        text={
          <p>
            One line, the field and the lockup. The field fills the top, the headline sits on clear
            background below it, and the lockup closes the composition in the corner, inset by the
            logo&apos;s clear space.
          </p>
        }
      >
        <Panel tone="sunken">
          <div className="flex justify-center p-6 sm:p-10">
            <div className="relative aspect-[1/1.414] w-full max-w-[22rem] overflow-hidden rounded-[4px] border border-line bg-ground [container-type:inline-size]">
              <StaticField cols={30} rows={30} time={1.4} className="absolute inset-x-0 top-0 h-[58%] w-full" />
              <div className="absolute inset-x-0 top-[38%] h-[24%] bg-[linear-gradient(to_bottom,transparent,#000000)]" />
              <div className="absolute inset-x-0 bottom-0 flex flex-col gap-[7cqw] p-[8cqw]">
                <p className="type-display text-[9.6cqw] text-fg">Permission, not trust</p>
                <div className="flex items-end justify-between">
                  <Lockup fontSize={8} />
                  <span className="font-mono text-[2.8cqw] uppercase tracking-[0.2em] text-muted-2">Open source</span>
                </div>
              </div>
            </div>
          </div>
        </Panel>
      </Part>

      <MoreSections slug="in-use" />
    </article>
  );
}
