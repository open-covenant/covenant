import type { Metadata } from "next";
import { CovenantMark } from "../../_brand";
import { MoreSections, PageHead, Panel, Part, Points, Rule, RuleGrid } from "../_kit";
import { Lockup, PartnerLockup, PartnerMark, StaticField } from "../_visuals";

const DESCRIPTION =
  "Co-branding with Covenant: the partner lockup, its construction, who leads, announcement cards and what to avoid.";

export const metadata: Metadata = {
  title: "Partnerships: Covenant brand guidelines",
  description: DESCRIPTION,
  alternates: { canonical: "/brand/partnerships" },
  openGraph: {
    type: "website",
    url: "https://opencovenant.org/brand/partnerships",
    title: "Partnerships · Covenant brand guidelines",
    description: DESCRIPTION,
    images: [{ url: "/opengraph-image.jpg", width: 1200, height: 630 }],
  },
};

export default function BrandPartnerships() {
  return (
    <article>
      <PageHead
        slug="partnerships"
        intro={
          <p>
            Covenant is open infrastructure: other products build on it, and it builds on theirs. When two
            brands appear together, each keeps its own identity. The partner lockup places them side by side,
            equal in weight, on Covenant&apos;s dark background.
          </p>
        }
      >
        <Panel tone="ground" caption="Partner marks shown are placeholders.">
          <div className="flex aspect-[16/8] items-center justify-center px-6 sm:aspect-[16/6]">
            <PartnerLockup fontSize={13} />
          </div>
        </Panel>
      </PageHead>

      <Part
        id="lockup"
        kicker="Partnerships"
        title="Partner lockup"
        text={
          <>
            <p>
              The Covenant lockup, a multiplication sign in Subtle text and the partner&apos;s logo. Use the
              partner&apos;s light or one-color version; both sit on the Covenant background.
            </p>
            <p>Where the Covenant name is already on screen, the mark alone can replace the lockup.</p>
          </>
        }
      >
        <div className="grid gap-4 sm:grid-cols-2">
          <Panel tone="ground" label="Full">
            <div className="flex h-40 items-center justify-center px-4">
              <PartnerLockup fontSize={9} />
            </div>
          </Panel>
          <Panel tone="ground" label="Compact">
            <div className="flex h-40 items-center justify-center gap-[1.4rem] px-4">
              <CovenantMark className="h-[30px] w-auto" />
              <span aria-hidden className="font-mono text-[15px] font-light text-muted-2">
                ×
              </span>
              <PartnerMark fontSize={11} />
            </div>
          </Panel>
        </div>
      </Part>

      <Part
        id="construction"
        kicker="Partnerships"
        title="Construction"
        text={
          <>
            <p>
              The space on each side of the × is two thirds of the Covenant mark&apos;s height (2/3 H). Match
              the partner&apos;s mark to the lockup&apos;s visual weight by eye, and align centers, not edges.
            </p>
            <p>Keep the logo&apos;s clear space, a fifth of the mark&apos;s height, around the whole group.</p>
          </>
        }
      >
        <Panel tone="ground" label="Optical, centered">
          <div className="relative flex h-56 items-center justify-center px-6">
            <span aria-hidden className="absolute inset-x-6 top-1/2 border-t border-dashed border-line-strong" />
            <span className="relative">
              <PartnerLockup fontSize={12} />
            </span>
          </div>
        </Panel>
      </Part>

      <Part
        id="order"
        kicker="Partnerships"
        title="Who leads"
        text={
          <p>
            Whoever announces goes first. On Covenant&apos;s own channels the Covenant lockup leads; on a
            partner&apos;s launch, theirs does, and Covenant follows at the same size.
          </p>
        }
      >
        <div className="grid gap-4 sm:grid-cols-2">
          <Panel tone="ground" label="Covenant announces">
            <div className="flex h-36 items-center justify-center px-4">
              <PartnerLockup fontSize={8.5} />
            </div>
          </Panel>
          <Panel tone="ground" label="Partner announces">
            <div className="flex h-36 items-center justify-center px-4">
              <PartnerLockup fontSize={8.5} partnerFirst />
            </div>
          </Panel>
        </div>
      </Part>

      <Part
        id="announcements"
        kicker="Partnerships"
        title="Announcements"
        wide
        text={
          <p>
            Integration cards follow the share card: the dark background, a band of the field, the partner
            lockup and one plain line describing what the integration does.
          </p>
        }
      >
        <Panel tone="ground" label="1200 × 630 · integration card">
          <div className="relative flex aspect-[1200/630] flex-col justify-between overflow-hidden p-[5%]">
            <div className="flex items-center justify-between">
              <PartnerLockup fontSize={8} />
              <span className="font-mono text-[8px] uppercase tracking-[0.2em] text-muted-2 sm:text-[10px]">Integration</span>
            </div>
            <StaticField strip cols={80} rows={10} time={2.9} className="absolute inset-x-0 top-[34%] h-[26%] w-full" />
            <p className="type-display relative max-w-[20ch] text-[clamp(1.1rem,3.4vw,2.4rem)] text-fg">
              Partner agents run under Covenant grants
            </p>
          </div>
        </Panel>
        <div className="mt-6">
          <Points
            items={[
              <>State what the integration does, not that it is exciting.</>,
              <>Name both products in full on first mention, then the short names.</>,
              <>Link the docs, the repo or the transaction that proves it is live.</>,
            ]}
          />
        </div>
      </Part>

      <Part id="misuse" kicker="Partnerships" title="Misuse" wide>
        <RuleGrid>
          <Rule ok={false} caption="Box or badge the partner's mark">
            <span className="flex items-center gap-4">
              <Lockup fontSize={7} />
              <span className="font-mono text-[12px] font-light text-muted-2">×</span>
              <span className="rounded-[6px] border border-line-strong bg-raised px-3 py-2.5">
                <PartnerMark fontSize={6} />
              </span>
            </span>
          </Rule>
          <Rule ok={false} caption="Recolor the partner's mark">
            <span className="flex items-center gap-4">
              <Lockup fontSize={7} />
              <span className="font-mono text-[12px] font-light text-muted-2">×</span>
              <span className="text-ramp font-sans text-[15px] font-semibold">Partner</span>
            </span>
          </Rule>
          <Rule ok={false} caption="Size the marks unequally">
            <span className="flex items-center gap-3">
              <Lockup fontSize={5} />
              <span className="font-mono text-[10px] font-light text-muted-2">×</span>
              <PartnerMark fontSize={13} />
            </span>
          </Rule>
          <Rule ok={false} caption="Merge the marks">
            <span className="relative flex h-[40%] items-center">
              <CovenantMark className="h-full w-auto" />
              <span className="absolute left-[38%] top-1/2 h-[46%] w-[46%] -translate-y-1/2 rounded-full border-[3px] border-fg-2" />
            </span>
          </Rule>
        </RuleGrid>
      </Part>

      <MoreSections slug="partnerships" />
    </article>
  );
}
