import type { Metadata } from "next";
import { CovenantMark } from "../../_brand";
import { ColorCard } from "../ColorCard";
import { CodeBlock, MoreSections, PageHead, Panel, Part, Points, Rule, RuleGrid } from "../_kit";
import { BRAND_COLORS, GRADIENT_CSS, NEUTRALS, STATUS, TEXT_COLORS } from "../_palette";

const DESCRIPTION =
  "Covenant's color palette: violet, blue and mint against a deep, dark background. Neutrals, text colors, the Covenant gradient and its four uses, status colors, proportion, contrast and tokens.";

export const metadata: Metadata = {
  title: "Color: Covenant brand guidelines",
  description: DESCRIPTION,
  alternates: { canonical: "/brand/color" },
  openGraph: {
    type: "website",
    url: "https://opencovenant.org/brand/color",
    title: "Color · Covenant brand guidelines",
    description: DESCRIPTION,
    images: [{ url: "/opengraph-image.jpg", width: 1200, height: 630 }],
  },
};

const TOKENS = `/* Covenant color tokens · https://opencovenant.org/brand/color
   Designed for dark backgrounds. Text colors are for text only, never fills. */
:root {
  /* Brand */
  --covenant-violet: #a03bf0; /* Violet */
  --covenant-blue: #4c7fde; /* Blue */
  --covenant-mint: #1de3ae; /* Mint */
  --covenant-gradient: linear-gradient(115deg, #a03bf0, #4c7fde, #1de3ae);

  /* Neutrals */
  --covenant-background: #0d0b16; /* Background */
  --covenant-inset: #08070e; /* Inset */
  --covenant-surface: #14111f; /* Surface */
  --covenant-border-subtle: #1d1a2b; /* Border subtle */
  --covenant-border: #2a2640; /* Border */
  --covenant-border-strong: #3d3757; /* Border strong */

  /* Text */
  --covenant-text: #f3f1fa; /* Primary text */
  --covenant-text-secondary: #dedaee; /* Secondary text */
  --covenant-text-body: #c4bfd8; /* Body text */
  --covenant-text-muted: #a49ec0; /* Muted text */
  --covenant-text-subtle: #8079a0; /* Subtle text */
  --covenant-text-faint: #5c5675; /* Faint text */

  /* Status */
  --covenant-success: #1de3ae; /* Success */
  --covenant-warning: #e0b968; /* Warning */
  --covenant-error: #e18383; /* Error */
}`;

const PROPORTION = [
  { name: "Background", share: 82, className: "bg-ground" },
  { name: "Surfaces & borders", share: 9, className: "bg-raised" },
  { name: "Text", share: 7, className: "bg-[repeating-linear-gradient(90deg,#c4bfd8_0_2px,transparent_2px_5px)]" },
  { name: "Gradient", share: 2, className: "bg-ramp" },
];

function level(ratio: number) {
  if (ratio >= 7) return "AAA";
  if (ratio >= 4.5) return "AA";
  if (ratio >= 3) return "Large text";
  return "Decorative";
}

export default function BrandColor() {
  const contrastRows = [...TEXT_COLORS, ...BRAND_COLORS];
  return (
    <article>
      <PageHead
        slug="color"
        intro={
          <>
            <p>
              Violet, blue and mint against a deep, dark background. Covenant is designed for dark
              backgrounds only: there is no light theme, and nothing we make sits on white or light gray, on
              screen or in print.
            </p>
            <p>
              On that background, the Covenant gradient carries all of the brand color. Because it is used
              sparingly, it always draws the eye to what matters most.
            </p>
          </>
        }
      >
        <Panel tone="ground">
          <div className="grid h-56 grid-cols-[3fr_1fr_1fr_1fr_2fr] sm:h-72">
            <span className="flex items-end p-4 font-mono text-[10px] uppercase tracking-[0.18em] text-muted-2">Background</span>
            <span className="flex items-end bg-raised p-4 font-mono text-[10px] uppercase tracking-[0.18em] text-muted-2">Surface</span>
            <span className="flex items-end bg-line-soft p-4 font-mono text-[10px] uppercase tracking-[0.18em] text-muted-2">Border</span>
            <span className="flex items-end justify-center bg-ground pb-4">
              <span className="font-mono text-[2.4rem] font-medium leading-none text-fg">Aa</span>
            </span>
            <span className="flex items-end bg-ramp p-4 font-mono text-[10px] uppercase tracking-[0.18em] text-white">Gradient</span>
          </div>
        </Panel>
      </PageHead>

      <Part
        id="dark-backgrounds"
        kicker="Color"
        title="Dark backgrounds"
        text={
          <>
            <p>
              Every Covenant surface uses a dark background: the website, documentation, print, slides,
              share cards and wallet dialogs.
            </p>
            <p>
              Where a platform defaults to light (email clients, document tools, operating systems), set the
              background to #0D0B16.
            </p>
          </>
        }
      >
        <Points
          items={[
            <>
              <strong>No white or light backgrounds.</strong> Not for a section, card, button, dialog or printed
              page.
            </>,
            <>
              <strong>Light text only.</strong> Text colors are the only light colors, and they are never used
              as fills.
            </>,
            <>
              <strong>Depth through tone, not shadows.</strong> Inset, Background and Surface sit a few steps
              apart, and borders separate one element from the next.
            </>,
            <>
              <strong>Print stays dark.</strong> Print the background color, and proof the gradient on dark
              stock.
            </>,
          ]}
        />
      </Part>

      <Part
        id="neutrals"
        kicker="Color"
        title="Neutrals"
        wide
        text={
          <p>
            Six violet-tinted neutrals. Background is the page color; the other five separate the elements
            placed on it.
          </p>
        }
      >
        <div className="grid gap-3 sm:grid-cols-2 lg:grid-cols-3">
          {NEUTRALS.map((c) => (
            <ColorCard key={c.token} color={c} />
          ))}
        </div>
        <Panel tone="ground" label="Layered" outerClassName="mt-6">
          <div className="p-5 sm:p-8">
            <div className="rounded-[6px] border border-line bg-raised p-5">
              <p className="font-mono text-[10px] uppercase tracking-[0.2em] text-muted-2">Surface · a card</p>
              <p className="type-display mt-3 text-[1.3rem] text-fg">Audit log</p>
              <div className="mt-4 rounded-[6px] border border-line bg-sunken p-4 font-mono text-[11.5px] text-fg-soft">
                <span className="text-muted-2">$</span> covenant audit verify
              </div>
              <p className="mt-3 font-mono text-[10px] uppercase tracking-[0.2em] text-muted-2">Inset · code inside it</p>
            </div>
          </div>
        </Panel>
      </Part>

      <Part
        id="text"
        kicker="Color"
        title="Text colors"
        wide
        text={
          <p>
            Six light tints, each leaning toward the violet of the background so text never glares.
            Contrast ratios are measured against Background. Subtle text is the minimum for anything meant
            to be read.
          </p>
        }
      >
        <div className="grid gap-3 sm:grid-cols-2 lg:grid-cols-3">
          {TEXT_COLORS.map((c) => (
            <ColorCard key={c.token} color={c} kind="type" />
          ))}
        </div>
      </Part>

      <Part
        id="gradient"
        kicker="Color"
        title="The gradient"
        wide
        text={
          <>
            <p>
              The Covenant gradient runs from violet to blue to mint at 115°, across the element it fills, with
              two exceptions: in the mark it runs corner to corner, and in the bands between sections it runs
              straight across. The order of the colors never changes.
            </p>
            <p>
              It has four uses and no others: <strong>the mark</strong>, <strong>the field</strong>,{" "}
              <strong>the primary action</strong> and <strong>one accent per view</strong> (a word in a
              headline, a line in a chart, a marker in a list).
            </p>
          </>
        }
      >
        <div className="grid gap-3 sm:grid-cols-3">
          {BRAND_COLORS.map((c) => (
            <ColorCard key={c.token} color={c} />
          ))}
        </div>
        <Panel tone="ground" label="115° · 0 / 50 / 100%" outerClassName="mt-6">
          <div className="h-24 bg-ramp" />
          <div className="grid grid-cols-3 border-t border-line font-mono text-[10px] uppercase tracking-[0.16em] text-muted">
            <span className="p-3">#A03BF0 · 0%</span>
            <span className="p-3 text-center">#4C7FDE · 50%</span>
            <span className="p-3 text-right">#1DE3AE · 100%</span>
          </div>
        </Panel>
        <div className="mt-6 grid gap-4 md:grid-cols-2">
          <div className="grid grid-cols-2 gap-3">
            <Panel tone="ground" label="The mark">
              <div className="flex aspect-square items-center justify-center">
                <CovenantMark className="h-[46%] w-auto" />
              </div>
            </Panel>
            <Panel tone="ground" label="The primary action">
              <div className="flex aspect-square items-center justify-center">
                <span className="btn btn-primary pointer-events-none">Try the sandbox →</span>
              </div>
            </Panel>
            <Panel tone="ground" label="One word">
              <div className="flex aspect-square items-center justify-center p-4">
                <p className="type-display text-center text-[1.35rem] text-fg">
                  Try the <span className="text-ramp">sandbox</span>
                </p>
              </div>
            </Panel>
            <Panel tone="ground" label="One marker">
              <div className="flex aspect-square items-center justify-center p-4">
                <ul className="flex flex-col gap-2.5 font-mono text-[11px] text-fg-soft">
                  {["Intent", "Grant", "Receipt"].map((t, i) => (
                    <li key={t} className="flex items-center gap-2.5">
                      <span className={`h-1.5 w-1.5 rounded-[1.5px] ${i === 1 ? "bg-ramp" : "bg-line-strong"}`} />
                      {t}
                    </li>
                  ))}
                </ul>
              </div>
            </Panel>
          </div>
          <CodeBlock label="the gradient CSS" code={`background-image: ${GRADIENT_CSS.toLowerCase()};\n\n/* as text, for one word */\nbackground-image: ${GRADIENT_CSS.toLowerCase()};\n-webkit-background-clip: text;\nbackground-clip: text;\ncolor: transparent;`} />
        </div>
      </Part>

      <Part
        id="status"
        kicker="Color"
        title="Status"
        wide
        text={<p>Product states only: live, in progress, failed. Shown as a dot or a word, never as decoration.</p>}
      >
        <div className="grid gap-3 sm:grid-cols-3">
          {STATUS.map((c) => (
            <ColorCard key={c.token} color={c} />
          ))}
        </div>
        <Panel tone="ground" label="In product" outerClassName="mt-6">
          <div className="flex flex-wrap gap-3 p-6">
            {[
              { label: "Live", dot: "bg-ok", text: "text-ok", ring: "border-ok/40 bg-ok/10" },
              { label: "In development", dot: "bg-warn", text: "text-warn", ring: "border-warn/40 bg-warn/10" },
              { label: "Failed", dot: "bg-bad", text: "text-bad", ring: "border-bad/40 bg-bad/10" },
            ].map((s) => (
              <span key={s.label} className={`inline-flex items-center gap-2 rounded-full border px-3 py-1.5 font-mono text-[10px] uppercase tracking-[0.18em] ${s.ring} ${s.text}`}>
                <span className={`h-1.5 w-1.5 rounded-full ${s.dot}`} />
                {s.label}
              </span>
            ))}
          </div>
        </Panel>
      </Part>

      <Part
        id="proportion"
        kicker="Color"
        title="Proportion"
        text={
          <p>
            Lead with the background. The gradient always takes the smallest share of the screen; that
            restraint is what makes it recognizably Covenant.
          </p>
        }
      >
        <Panel tone="raised">
          <div className="p-5 sm:p-6">
            <div className="flex h-16 overflow-hidden rounded-[4px] border border-line">
              {PROPORTION.map((p) => (
                <span key={p.name} className={p.className} style={{ width: `${p.share}%` }} />
              ))}
            </div>
            <ul className="mt-5 grid grid-cols-2 gap-3 sm:grid-cols-4">
              {PROPORTION.map((p) => (
                <li key={p.name} className="flex flex-col gap-1">
                  <span className="font-mono text-[18px] tabular-nums text-fg">~{p.share}%</span>
                  <span className="font-mono text-[10px] uppercase tracking-[0.16em] text-muted-2">{p.name}</span>
                </li>
              ))}
            </ul>
          </div>
        </Panel>
      </Part>

      <Part
        id="contrast"
        kicker="Color"
        title="Contrast"
        wide
        text={
          <p>
            WCAG 2 contrast of each color against Background (#0D0B16). Body text needs AA (4.5:1); anything
            below 3:1 is decorative only.
          </p>
        }
      >
        <div className="overflow-x-auto rounded-[6px] border border-line">
          <table className="w-full min-w-[36rem] border-collapse text-left">
            <thead>
              <tr className="bg-raised">
                {["Sample", "Color", "Ratio", "Level", "Use for"].map((h) => (
                  <th key={h} className="border-b border-line px-4 py-3 font-mono text-[10px] font-medium uppercase tracking-[0.18em] text-muted">
                    {h}
                  </th>
                ))}
              </tr>
            </thead>
            <tbody>
              {contrastRows.map((c) => (
                <tr key={c.token}>
                  <td className="border-b border-line-soft px-4 py-3">
                    <span className="font-mono text-[1.3rem] font-medium" style={{ color: c.hex }}>
                      Aa
                    </span>
                  </td>
                  <td className="border-b border-line-soft px-4 py-3 font-mono text-[11.5px] text-fg">
                    {c.name} <span className="text-muted-2">{c.hex}</span>
                  </td>
                  <td className="border-b border-line-soft px-4 py-3 font-mono text-[11.5px] tabular-nums text-fg-soft">{c.contrast.toFixed(2)}:1</td>
                  <td className="border-b border-line-soft px-4 py-3 font-mono text-[10px] uppercase tracking-[0.16em] text-muted">{level(c.contrast)}</td>
                  <td className="border-b border-line-soft px-4 py-3 text-[12px] text-muted">{c.role}</td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      </Part>

      <Part
        id="tokens"
        kicker="Color"
        title="Tokens"
        text={
          <p>
            The palette as CSS custom properties. The brand assets download includes the same set as CSS and
            JSON, with every value in HEX, RGB, CMYK and OKLCH.
          </p>
        }
      >
        <CodeBlock label="the color tokens" code={TOKENS} />
      </Part>

      <Part id="misuse" kicker="Color" title="Misuse" wide>
        <RuleGrid>
          <Rule ok={false} caption="Offer a light theme or a white background">
            <span className="flex items-center gap-3 rounded-full border border-line-strong px-4 py-2 font-mono text-[10px] uppercase tracking-[0.18em]">
              <span className="text-muted-2 line-through decoration-bad decoration-2">Light</span>
              <span className="h-4 w-8 rounded-full bg-line-strong p-0.5">
                <span className="block h-3 w-3 rounded-full bg-fg-soft" />
              </span>
              <span className="text-fg">Dark</span>
            </span>
          </Rule>
          <Rule ok={false} caption="Reverse, rotate or extend the gradient">
            <span className="h-10 w-[70%] rounded-[4px] bg-[linear-gradient(115deg,#1de3ae,#4c7fde,#a03bf0,#e0b968)]" />
          </Rule>
          <Rule ok={false} caption="Fill large areas with the gradient" className="bg-ramp">
            <span className="type-display text-[1.1rem] text-white">Big news</span>
          </Rule>
          <Rule ok={false} caption="Tint the background with brand colors" className="bg-[#2b1150]">
            <CovenantMark className="h-[40%] w-auto" />
          </Rule>
          <Rule ok={false} caption="Set reading text in the gradient">
            <p className="text-ramp max-w-[80%] text-[11px] leading-relaxed">
              Every agent runs under a signed grant, and every action leaves a receipt in the audit log.
            </p>
          </Rule>
          <Rule ok={false} caption="Use status colors as decoration">
            <span className="type-display text-[1.25rem] text-warn">Covenant Desk</span>
          </Rule>
          <Rule ok={false} caption="Use Faint text for body copy">
            <p className="max-w-[80%] text-[11px] leading-relaxed text-faint">
              Every agent runs under a signed grant, and every action leaves a receipt in the audit log.
            </p>
          </Rule>
          <Rule ok={false} caption="Introduce other colors">
            <span className="flex gap-2">
              {["#ff7a3d", "#ffd23f", "#ff5fa2"].map((c) => (
                <span key={c} className="h-10 w-10 rounded-[6px]" style={{ backgroundColor: c }} />
              ))}
            </span>
          </Rule>
        </RuleGrid>
      </Part>

      <MoreSections slug="color" />
    </article>
  );
}
