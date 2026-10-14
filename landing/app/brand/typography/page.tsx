import type { Metadata } from "next";
import { TypeTester } from "../_demos";
import { MoreSections, PageHead, Panel, Part, Rule, RuleGrid, Spec } from "../_kit";

const DESCRIPTION =
  "Covenant's two typefaces: Archivo Expanded for display and JetBrains Mono for everything else. Specimens, a type tester, the type scale, character sets, fallbacks and misuse.";

export const metadata: Metadata = {
  title: "Typography: Covenant brand guidelines",
  description: DESCRIPTION,
  alternates: { canonical: "/brand/typography" },
  openGraph: {
    type: "website",
    url: "https://opencovenant.org/brand/typography",
    title: "Typography · Covenant brand guidelines",
    description: DESCRIPTION,
    images: [{ url: "/opengraph-image.jpg", width: 1200, height: 630 }],
  },
};

const UPPER = "ABCDEFGHIJKLMNOPQRSTUVWXYZ";
const LOWER = "abcdefghijklmnopqrstuvwxyz";
const DIGITS = "0123456789";
const DISPLAY_PUNCT = "&?!@#%$€£¥+−×÷=/(),.:;'\"«»—";
const MONO_EXTRA = "{}[]<>|\\/_~^*`→←↑↓·•§¶©®™✓✕";

const SCALE = [
  {
    name: "Display XL",
    spec: "Archivo Expanded · 600 · clamp(2.6rem, 7.4vw, 112px) · leading 0.8 · −0.06em",
    use: "The headline, once per page.",
    sample: <span className="type-display text-[clamp(2.2rem,5vw,4.4rem)] leading-[0.8] tracking-[-0.06em] text-fg">That builds itself</span>,
  },
  {
    name: "Page title",
    spec: "Archivo Expanded · 600 · clamp(2.3rem, 5.4vw, 4.2rem) · leading 0.88 · −0.05em",
    use: "The title of every subpage.",
    sample: <span className="page-title text-fg">Covenant Guard</span>,
  },
  {
    name: "Section title",
    spec: "Archivo Expanded · 600 · clamp(1.9rem, 3.8vw, 3.1rem) · leading 0.92 · −0.05em",
    use: "Opens a section; one accent word may take the gradient.",
    sample: (
      <span className="type-display text-[clamp(1.6rem,3vw,2.4rem)] text-fg">
        The infrastructure <span className="text-ramp">builds itself</span>
      </span>
    ),
  },
  {
    name: "Card title",
    spec: "Archivo Expanded · 600 · 1.6rem · leading 0.92 · −0.05em",
    use: "Names inside cards and panels.",
    sample: <span className="type-display text-[1.6rem] text-fg">Trading</span>,
  },
  {
    name: "Label",
    spec: "JetBrains Mono · 500 · 10–11px · capitals · tracking 0.2–0.28em",
    use: "Eyebrows, navigation, buttons, table heads.",
    sample: <span className="font-mono text-[10.5px] font-medium uppercase tracking-[0.28em] text-muted">Eight host-level primitives</span>,
  },
  {
    name: "Body",
    spec: "JetBrains Mono · 400 · 13px · leading 1.7",
    use: "Running text, ledes, descriptions.",
    sample: (
      <span className="max-w-[34rem] text-[13px] leading-[1.7] text-fg-soft">
        Covenant gives humans and agents eight host-level primitives, so they can safely share one computer.
      </span>
    ),
  },
  {
    name: "Small",
    spec: "JetBrains Mono · 400 · 11.5–12px · leading 1.6",
    use: "Captions, footnotes, metadata.",
    sample: <span className="text-[12px] leading-[1.6] text-muted">Open source · Apache-2.0</span>,
  },
  {
    name: "Code",
    spec: "JetBrains Mono · 400 · 12.5px · leading 1.7 · on Inset",
    use: "Commands, payloads, addresses.",
    sample: (
      <span className="rounded-[4px] border border-line bg-sunken px-3 py-2 font-mono text-[12.5px] text-fg-soft">
        covenant audit verify
      </span>
    ),
  },
];

export default function BrandTypography() {
  return (
    <article>
      <PageHead
        slug="typography"
        intro={
          <p>
            Covenant uses two typefaces, each with one job: Archivo Expanded sets every headline in semibold,
            tightly spaced capitals, and JetBrains Mono sets everything else, from body text, labels and
            navigation to figures and code.
          </p>
        }
      >
        <Panel tone="ground">
          <div className="grid sm:grid-cols-[1.4fr_1fr]">
            <div className="flex min-h-[16rem] flex-col justify-between gap-8 border-b border-line p-6 sm:border-b-0 sm:border-r sm:p-8">
              <span className="font-mono text-[10px] uppercase tracking-[0.22em] text-muted-2">Display · Archivo Expanded · 600</span>
              <span className="type-display text-[clamp(3rem,8vw,6.4rem)] leading-[0.8] text-fg">Agent</span>
            </div>
            <div className="flex min-h-[16rem] flex-col justify-between gap-8 p-6 sm:p-8">
              <span className="font-mono text-[10px] uppercase tracking-[0.22em] text-muted-2">Text · JetBrains Mono</span>
              <span className="font-mono text-[clamp(3.2rem,8vw,6rem)] font-medium leading-none tracking-[-0.02em] text-fg">Aa</span>
            </div>
          </div>
        </Panel>
      </PageHead>

      <Part
        id="typefaces"
        kicker="Typography"
        title="Typefaces"
        text={
          <p>
            Both are open source under the SIL Open Font License and free on Google Fonts. There is no
            license to buy.
          </p>
        }
      >
        <div className="grid gap-4 sm:grid-cols-2">
          <Panel tone="raised" label="Display">
            <div className="flex flex-col gap-4 p-5">
              <span className="type-display text-[2.2rem] text-fg">Archivo</span>
              <Spec
                rows={[
                  ["Set at", "Width 125 · weight 600"],
                  ["Tracking", "−0.05em (−0.06em at Display XL)"],
                  ["Leading", "0.88–0.92 (0.8 at Display XL)"],
                  ["Case", "Capitals, always"],
                ]}
              />
            </div>
          </Panel>
          <Panel tone="raised" label="Text">
            <div className="flex flex-col gap-4 p-5">
              <span className="font-mono text-[1.9rem] font-medium leading-none text-fg">JetBrains Mono</span>
              <Spec
                rows={[
                  ["Weights", "300 · 400 · 500 · 600"],
                  ["Body", "13px · leading 1.7"],
                  ["Labels", "Capitals · 0.2–0.28em"],
                  ["Figures", "Tabular in data"],
                ]}
              />
            </div>
          </Panel>
        </div>
      </Part>

      <Part
        id="display"
        kicker="Typography"
        title="Archivo Expanded"
        text={
          <>
            <p>
              Archivo is a variable grotesque with a width axis. Set at its widest, width 125, and weight 600,
              with tracking tightened to −0.05em, it becomes broad and dense, with the same solid presence as
              the mark.
            </p>
            <p>Headlines always appear in capitals. Write them in sentence case; the display style sets the capitals.</p>
          </>
        }
      >
        <Panel tone="ground" label="Archivo · width 125 · 600">
          <div className="flex flex-col gap-5 p-6 sm:p-8">
            <p className="type-display text-[clamp(1.8rem,4.2vw,3rem)] leading-[0.95] text-fg">{UPPER}</p>
            <p className="type-display text-[clamp(1.8rem,4.2vw,3rem)] leading-[0.95] text-fg">{DIGITS}</p>
            <p className="type-display break-all text-[clamp(1.4rem,3vw,2.2rem)] leading-[1.05] text-muted">{DISPLAY_PUNCT}</p>
          </div>
        </Panel>
      </Part>

      <Part
        id="mono"
        kicker="Typography"
        title="JetBrains Mono"
        text={
          <>
            <p>
              JetBrains Mono was designed for long hours of reading code. Covenant sets all running text in
              it, so documentation, interfaces and technical records read as one.
            </p>
            <p>400 for body, 500 for labels and names, 600 for emphasis.</p>
          </>
        }
      >
        <Panel tone="ground" label="JetBrains Mono · 300–600">
          <div className="flex flex-col divide-y divide-line-soft">
            {[
              ["300", "font-light"],
              ["400", "font-normal"],
              ["500", "font-medium"],
              ["600", "font-semibold"],
            ].map(([w, cls]) => (
              <div key={w} className="flex items-baseline gap-6 px-6 py-4">
                <span className="w-8 shrink-0 font-mono text-[10px] tabular-nums text-muted-2">{w}</span>
                <span className={`truncate font-mono text-[clamp(1.1rem,2.4vw,1.6rem)] text-fg ${cls}`}>intent · runtime · memory · identity</span>
              </div>
            ))}
          </div>
        </Panel>
      </Part>

      <Part
        id="tester"
        kicker="Typography"
        title="Type tester"
        wide
        text={<p>Display type is always set in capitals.</p>}
      >
        <Panel tone="ground">
          <TypeTester />
        </Panel>
      </Part>

      <Part id="scale" kicker="Typography" title="Type scale" wide text={<p>Eight styles cover every surface, with no sizes in between. The one exception is documentation body text, set at 13.5 px.</p>}>
        <ol className="flex flex-col border-t border-line">
          {SCALE.map((s) => (
            <li key={s.name} className="grid gap-4 border-b border-line py-6 md:grid-cols-[minmax(0,15rem)_minmax(0,1fr)] md:gap-10">
              <div className="flex flex-col gap-1.5">
                <span className="font-mono text-[11.5px] font-semibold uppercase tracking-[0.16em] text-fg">{s.name}</span>
                <span className="font-mono text-[10.5px] leading-relaxed text-muted-2">{s.spec}</span>
                <span className="text-[11.5px] leading-relaxed text-muted">{s.use}</span>
              </div>
              <div className="flex min-w-0 items-center">{s.sample}</div>
            </li>
          ))}
        </ol>
      </Part>

      <Part id="characters" kicker="Typography" title="Character set" wide text={<p>The Latin character sets in use.</p>}>
        <div className="grid gap-4 lg:grid-cols-2">
          <Panel tone="raised" label="Archivo Expanded">
            <div className="grid grid-cols-[repeat(auto-fill,minmax(2.6rem,1fr))] gap-px bg-line-soft">
              {[...UPPER, ...DIGITS, ..."&?!@#%$€£+=/"].map((c, i) => (
                <span key={`${c}${i}`} className="type-display flex aspect-square items-center justify-center bg-raised text-[1.15rem] text-fg">
                  {c}
                </span>
              ))}
            </div>
          </Panel>
          <Panel tone="raised" label="JetBrains Mono">
            <div className="grid grid-cols-[repeat(auto-fill,minmax(2.2rem,1fr))] gap-px bg-line-soft">
              {[...UPPER, ...LOWER, ...DIGITS, ...MONO_EXTRA].map((c, i) => (
                <span key={`${c}${i}`} className="flex aspect-square items-center justify-center bg-raised font-mono text-[0.95rem] text-fg">
                  {c}
                </span>
              ))}
            </div>
          </Panel>
        </div>
      </Part>

      <Part
        id="pairing"
        kicker="Typography"
        title="In combination"
        text={
          <p>
            A label for context, a headline for the point, body text to explain and a link to continue.
            Every section follows this order.
          </p>
        }
      >
        <Panel tone="ground">
          <div className="flex flex-col gap-5 p-6 sm:p-9">
            <p className="font-mono text-[10px] font-medium uppercase tracking-[0.28em] text-muted">Built in the open</p>
            <p className="type-display max-w-[16ch] text-[clamp(1.8rem,4vw,2.9rem)] text-fg">
              The infrastructure <span className="text-ramp">builds itself</span>
            </p>
            <p className="max-w-[36rem] text-[13px] leading-[1.7] text-muted">
              Covenant&apos;s own code is written in the open by agents running on it. Every commit is made
              under a signed grant, checked before it goes through, and leaves a receipt in a hash-chained
              audit log.
            </p>
            <span className="font-mono text-[10.5px] font-medium uppercase tracking-[0.2em] text-fg">
              Watch the live build log <span aria-hidden>→</span>
            </span>
          </div>
        </Panel>
      </Part>

      <Part
        id="fallbacks"
        kicker="Typography"
        title="Fallbacks & licensing"
        text={<p>When the brand typefaces cannot load, these take their place, in this order.</p>}
      >
        <Spec
          rows={[
            ["Display", "Archivo → Arial Black → system sans"],
            ["Text", "JetBrains Mono → ui-monospace → SF Mono → Menlo → Consolas"],
            ["License", "SIL Open Font License 1.1, both faces"],
            [
              "Get them",
              <>
                <a href="https://fonts.google.com/specimen/Archivo" target="_blank" rel="noopener noreferrer" className="text-fg underline decoration-line-strong underline-offset-[3px] hover:decoration-blue">
                  Archivo
                </a>
                {" · "}
                <a href="https://fonts.google.com/specimen/JetBrains+Mono" target="_blank" rel="noopener noreferrer" className="text-fg underline decoration-line-strong underline-offset-[3px] hover:decoration-blue">
                  JetBrains Mono
                </a>
              </>,
            ],
            ["On the web", "Self-host both fonts and use font-display: swap, so text shows in a fallback font until they load."],
          ]}
        />
      </Part>

      <Part id="misuse" kicker="Typography" title="Misuse" wide>
        <RuleGrid>
          <Rule ok={false} caption="Set body copy in Archivo">
            <p className="max-w-[80%] font-display text-[12px] leading-relaxed text-fg-soft">
              Every agent runs under a signed grant, and every action leaves a receipt.
            </p>
          </Rule>
          <Rule ok={false} caption="Set display type in lowercase">
            <span className="font-display text-[1.35rem] font-semibold tracking-[-0.05em] text-fg" style={{ fontVariationSettings: '"wdth" 125' }}>
              builds itself
            </span>
          </Rule>
          <Rule ok={false} caption="Set Archivo at its normal width">
            <span className="font-display text-[1.35rem] font-semibold uppercase tracking-[-0.05em] text-fg" style={{ fontVariationSettings: '"wdth" 100' }}>
              Builds itself
            </span>
          </Rule>
          <Rule ok={false} caption="Open up display tracking">
            <span className="type-display text-[1.1rem] text-fg" style={{ letterSpacing: "0.14em" }}>
              Builds itself
            </span>
          </Rule>
          <Rule ok={false} caption="Use a light display weight">
            <span className="type-display text-[1.35rem] text-fg" style={{ fontWeight: 300 }}>
              Builds itself
            </span>
          </Rule>
          <Rule ok={false} caption="Add a third typeface">
            <span className="font-serif text-[1.6rem] italic text-fg">Builds itself</span>
          </Rule>
          <Rule ok={false} caption="Track lowercase running text">
            <p className="max-w-[80%] text-[11px] leading-relaxed tracking-[0.22em] text-fg-soft">every agent runs under a signed grant</p>
          </Rule>
          <Rule ok={false} caption="Outline headlines">
            <span className="type-display text-[1.5rem] text-transparent [-webkit-text-stroke:1px_#f3f1fa]">Builds itself</span>
          </Rule>
        </RuleGrid>
      </Part>

      <MoreSections slug="typography" />
    </article>
  );
}
