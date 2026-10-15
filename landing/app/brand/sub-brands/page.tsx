import type { Metadata } from "next";
import { pageMetadata } from "@/app/_metadata";
import { CovenantMark } from "../../_brand";
import { MoreSections, PageHead, Panel, Part, Points, Rule, RuleGrid } from "../_kit";
import { SubBrandLockup } from "../_visuals";

const DESCRIPTION =
  "How Covenant names and locks up its products: Covenant plus one descriptor, the same mark, the same type, no new logos.";

export const metadata: Metadata = pageMetadata("/brand/sub-brands", { title: "Sub-brands", section: "Brand guidelines" }, {
  title: "Sub-brands: Covenant brand guidelines",
  description: DESCRIPTION,
  alternates: { canonical: "/brand/sub-brands" },
  openGraph: {
    type: "website",
    url: "https://opencovenant.org/brand/sub-brands",
    title: "Sub-brands · Covenant brand guidelines",
    description: DESCRIPTION,
  },
});

const FAMILY = [
  { name: "Guard", what: "A zero-install MCP server that checks another agent's track record, identity and signed claims before your agent trusts or pays it.", where: "opencovenant.org/guard" },
  { name: "Compute", what: "Rent a GPU by the second and pay in USDC.", where: "opencovenant.org/compute" },
  { name: "Desk", what: "A local trading desk for Robinhood Chain stock tokens, run by your agent over MCP.", where: "opencovenant.org/desk" },
  { name: "Sandbox", what: "Covenant in the browser, with nothing to install.", where: "sandbox.opencovenant.org" },
  { name: "Stake", what: "Lock $CVNT and share in Covenant protocol revenue, paid in SOL.", where: "stake.opencovenant.org" },
  { name: "Docs", what: "Concepts, the CLI, the HTTP API and the protocols.", where: "docs.opencovenant.org" },
];

export default function BrandSubBrands() {
  return (
    <article>
      <PageHead
        slug="sub-brands"
        intro={
          <p>
            Every Covenant product is named Covenant plus one plain descriptor. The mark, the colors and the
            typography stay the same across all of them. A product earns a descriptor, never its own logo.
          </p>
        }
      >
        <Panel tone="ground">
          <div className="flex flex-col items-start gap-7 px-6 py-10 sm:px-10 sm:py-14">
            {["Guard", "Compute", "Desk"].map((d) => (
              <SubBrandLockup key={d} descriptor={d} fontSize={14} />
            ))}
          </div>
        </Panel>
      </PageHead>

      <Part
        id="structure"
        kicker="Sub-brands"
        title="Structure"
        text={
          <>
            <p>
              Naming is <strong>Covenant + descriptor</strong>: one word for what the product does. In running
              text, use the full name first (Covenant Guard), then the descriptor alone (Guard).
            </p>
            <p>
              A new product gets a descriptor when it has its own audience and its own surface. A feature of
              an existing product does not get a name of its own.
            </p>
          </>
        }
      >
        <ul className="grid gap-3 sm:grid-cols-2">
          {FAMILY.map((f) => (
            <li key={f.name} className="flex flex-col gap-3 rounded-[6px] border border-line bg-raised p-5">
              <span className="font-mono text-[12px] font-semibold uppercase tracking-[0.2em] text-fg">
                Covenant <span className="text-muted">{f.name}</span>
              </span>
              <span className="text-[12px] leading-relaxed text-muted">{f.what}</span>
              <span className="mt-auto font-mono text-[10.5px] text-muted-2">{f.where}</span>
            </li>
          ))}
        </ul>
      </Part>

      <Part
        id="lockups"
        kicker="Sub-brands"
        title="Lockups"
        wide
        text={<p>Every product lockup is the Covenant lockup with its descriptor added. Nothing else changes.</p>}
      >
        <div className="grid gap-3 sm:grid-cols-2">
          {FAMILY.map((f) => (
            <Panel key={f.name} tone="ground">
              <div className="flex h-36 items-center justify-center px-6">
                <SubBrandLockup descriptor={f.name} fontSize={11} />
              </div>
            </Panel>
          ))}
        </div>
      </Part>

      <Part
        id="construction"
        kicker="Sub-brands"
        title="Construction"
        text={
          <p>
            After the name, leave the same H/3 gap, draw a vertical rule in Border strong at 1.5 times the
            height of the name, and leave another H/3 gap before the descriptor. Set the descriptor in Muted
            text, in the same typeface, size and tracking as the name.
          </p>
        }
      >
        <Panel tone="ground" label="Measured from the lockup">
          <div className="flex flex-col items-center gap-6 px-6 py-12">
            <SubBrandLockup descriptor="Guard" fontSize={18} />
            <ul className="grid w-full max-w-[34rem] grid-cols-2 gap-3 font-mono text-[10px] uppercase tracking-[0.16em] text-muted-2 sm:grid-cols-4">
              <li>Mark · H</li>
              <li>Gaps · H/3</li>
              <li>Rule · Border strong</li>
              <li>Descriptor · Muted text</li>
            </ul>
          </div>
        </Panel>
      </Part>

      <Part
        id="in-product"
        kicker="Sub-brands"
        title="In product"
        text={
          <p>
            Inside a product, the name can be dropped; the mark, the rule and the descriptor identify it.
            Everywhere else (posts, slides, partner pages), use the full lockup.
          </p>
        }
      >
        <div className="grid gap-4 sm:grid-cols-2">
          <Panel tone="raised" label="In product">
            <div className="flex h-36 flex-col justify-center gap-5 px-6">
              <div className="flex items-center justify-between rounded-[6px] border border-line bg-ground px-4 py-3">
                <SubBrandLockup descriptor="Stake" withName={false} fontSize={10} />
                <span className="rounded-full border border-line-strong px-3 py-1.5 font-mono text-[9px] uppercase tracking-[0.18em] text-fg">Connect</span>
              </div>
            </div>
          </Panel>
          <Panel tone="raised" label="Elsewhere">
            <div className="flex h-36 items-center justify-center px-6">
              <SubBrandLockup descriptor="Stake" fontSize={11} />
            </div>
          </Panel>
        </div>
        <div className="mt-6">
          <Points
            items={[
              <>Use the short form only when the full lockup is visible in the same view, or when the web address already includes Covenant.</>,
              <>Products use the Covenant app icon. There are no product icons.</>,
            ]}
          />
        </div>
      </Part>

      <Part id="misuse" kicker="Sub-brands" title="Misuse" wide>
        <RuleGrid>
          <Rule ok={false} caption="Give a product its own mark">
            <span className="flex items-center gap-3 font-mono text-[11px] font-medium uppercase tracking-[0.24em] text-fg">
              <span className="flex h-8 w-8 items-center justify-center rounded-full bg-mint text-[14px] text-ground">G</span>
              Guard
            </span>
          </Rule>
          <Rule ok={false} caption="Color the descriptor">
            <span className="flex items-center gap-2.5 font-mono text-[10px] font-medium uppercase tracking-[0.24em] text-fg">
              <CovenantMark className="h-7 w-auto" />
              Covenant <span className="text-ramp">Guard</span>
            </span>
          </Rule>
          <Rule ok={false} caption="Enlarge the descriptor">
            <span className="flex items-center gap-2.5 text-fg">
              <CovenantMark className="h-5 w-auto" />
              <span className="font-mono text-[7px] font-medium uppercase tracking-[0.24em]">Covenant</span>
              <span className="type-display text-[1.6rem]">Guard</span>
            </span>
          </Rule>
          <Rule ok={false} caption="Abbreviate the name">
            <span className="flex items-center gap-2.5 font-mono text-[11px] font-medium uppercase tracking-[0.24em] text-fg">
              <CovenantMark className="h-7 w-auto" />
              CVNT Guard
            </span>
          </Rule>
        </RuleGrid>
      </Part>

      <MoreSections slug="sub-brands" />
    </article>
  );
}
