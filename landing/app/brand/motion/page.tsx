import type { Metadata } from "next";
import { pageMetadata } from "@/app/_metadata";
import { CovenantMark } from "../../_brand";
import { CurtainDemo, ResolveDemo, WordRevealDemo } from "../_demos";
import { CodeBlock, MoreSections, PageHead, Panel, Part, Points, Rule, RuleGrid } from "../_kit";

const DESCRIPTION =
  "How Covenant moves: one easing curve, a short set of durations, the word reveal, the opening animation, the field forming the mark, and reduced motion.";

export const metadata: Metadata = pageMetadata("/brand/motion", { title: "Motion", section: "Brand guidelines" }, {
  title: "Motion: Covenant brand guidelines",
  description: DESCRIPTION,
  alternates: { canonical: "/brand/motion" },
  openGraph: {
    type: "website",
    url: "https://opencovenant.org/brand/motion",
    title: "Motion · Covenant brand guidelines",
    description: DESCRIPTION,
  },
});

const DURATIONS = [
  { what: "Hover", value: "150 ms", detail: "Buttons rise 2 px; the primary button brightens 12%." },
  { what: "Header border", value: "200 ms", detail: "Appears below the header once the page scrolls." },
  { what: "Reveal", value: "700 ms", detail: "Content fades in and rises 18 px as it enters." },
  { what: "Word reveal", value: "780 ms", detail: "Headline words rise into view, 55 ms apart." },
  { what: "Opening animation", value: "620 ms", detail: "Per square; the sweep crosses the screen in about 0.9 s." },
  { what: "Forming the mark", value: "Scroll", detail: "Follows the scroll of the home page, eased at both ends." },
  { what: "Field", value: "Continuous", detail: "A slow, steady drift; each shape grows and fades at its own pace." },
  { what: "Section bands", value: "Continuous", detail: "Move with the scroll and respond to its speed." },
  { what: "Side menu", value: "600 ms", detail: "The menu slides in from the right, and its items rise into place one by one, 55 ms apart." },
];

/** A cubic-bezier curve plotted in a unit box. */
function Curve({ p1, p2, className = "", stroke = "url(#curve-ramp)" }: { p1: [number, number]; p2: [number, number]; className?: string; stroke?: string }) {
  const s = 200;
  const pt = ([x, y]: [number, number]) => `${x * s} ${s - y * s}`;
  return (
    <svg viewBox="-24 -60 248 320" className={className} aria-hidden>
      <defs>
        <linearGradient id="curve-ramp" x1="0" y1="200" x2="200" y2="0" gradientUnits="userSpaceOnUse">
          <stop offset="0" stopColor="#a03bf0" />
          <stop offset="0.5" stopColor="#4c7fde" />
          <stop offset="1" stopColor="#1de3ae" />
        </linearGradient>
      </defs>
      <rect x="0" y="0" width={s} height={s} fill="none" stroke="#2a2640" strokeWidth="1.5" />
      {[0.25, 0.5, 0.75].map((g) => (
        <g key={g} stroke="#1d1a2b" strokeWidth="1">
          <line x1={g * s} y1="0" x2={g * s} y2={s} />
          <line x1="0" y1={g * s} x2={s} y2={g * s} />
        </g>
      ))}
      <line x1="0" y1={s} x2={p1[0] * s} y2={s - p1[1] * s} stroke="#5c5675" strokeWidth="1.5" strokeDasharray="4 4" />
      <line x1={s} y1="0" x2={p2[0] * s} y2={s - p2[1] * s} stroke="#5c5675" strokeWidth="1.5" strokeDasharray="4 4" />
      <circle cx={p1[0] * s} cy={s - p1[1] * s} r="5" fill="#a49ec0" />
      <circle cx={p2[0] * s} cy={s - p2[1] * s} r="5" fill="#a49ec0" />
      <path d={`M0 ${s} C${pt(p1)} ${pt(p2)} ${s} 0`} fill="none" stroke={stroke} strokeWidth="4" strokeLinecap="round" />
    </svg>
  );
}

export default function BrandMotion() {
  return (
    <article>
      <PageHead
        slug="motion"
        intro={
          <p>
            Every Covenant animation has a destination. The field comes together to form the mark, words rise
            into place and a grid of squares clears to reveal the page. Motion shows where something came from
            and where it belongs, and it is never there for show.
          </p>
        }
      >
        <Panel tone="ground">
          <ResolveDemo className="aspect-[16/10] w-full sm:aspect-[16/7]" />
        </Panel>
      </PageHead>

      <Part id="principles" kicker="Motion" title="Principles" text={<p>Six rules every animation follows.</p>}>
        <Points
          items={[
            <>
              <strong>Arrive, then rest.</strong> Every animation moves toward a final state, then stops. The
              field&apos;s slow drift is the one exception. Nothing loops to compete for attention.
            </>,
            <>
              <strong>One curve.</strong> Everything that enters uses the same ease-out: a quick start and a long,
              soft landing.
            </>,
            <>
              <strong>Quick to touch, unhurried to reveal.</strong> 150 ms answers a pointer; 600–800 ms brings
              content in.
            </>,
            <>
              <strong>Built from the mark.</strong> Motion uses the same squares as the logo: a grid clearing,
              shapes growing, the field drifting. Never a spin, a bounce or a 3D flip.
            </>,
            <>
              <strong>Nothing plays unseen.</strong> Animations pause when they are out of view or the tab is
              hidden.
            </>,
            <>
              <strong>Reduced motion loses nothing.</strong> With it on, every element lands in its final state at once.
            </>,
          ]}
        />
      </Part>

      <Part
        id="easing"
        kicker="Motion"
        title="The curve"
        text={
          <>
            <p>
              <strong>cubic-bezier(0.16, 1, 0.3, 1)</strong>, an exponential ease-out. Most of the distance is
              covered in the first third of the time; the rest is a long, gentle finish.
            </p>
            <p>Hover and color changes use a standard ease at 150 ms.</p>
          </>
        }
      >
        <div className="grid gap-4 md:grid-cols-[minmax(0,16rem)_minmax(0,1fr)]">
          <Panel tone="ground" label="0.16, 1, 0.3, 1">
            <div className="flex items-center justify-center p-4">
              <Curve p1={[0.16, 1]} p2={[0.3, 1]} className="h-auto w-full max-w-[14rem]" />
            </div>
          </Panel>
          <div className="flex flex-col gap-4">
            <Panel tone="ground" label="Against linear">
              <div className="flex flex-col gap-5 px-5 py-6">
                <div>
                  <span className="font-mono text-[10px] uppercase tracking-[0.18em] text-fg">Covenant</span>
                  <div className="relative mt-2 h-8 rounded-full border border-line-soft">
                    <span className="brand-run bg-ramp" />
                  </div>
                </div>
                <div>
                  <span className="font-mono text-[10px] uppercase tracking-[0.18em] text-muted-2">Linear</span>
                  <div className="relative mt-2 h-8 rounded-full border border-line-soft">
                    <span className="brand-run is-linear bg-line-strong" />
                  </div>
                </div>
              </div>
            </Panel>
            <CodeBlock label="the easing" code={`--ease: cubic-bezier(0.16, 1, 0.3, 1);\n\ntransition: transform 0.7s var(--ease),\n            opacity 0.7s var(--ease);`} />
          </div>
        </div>
      </Part>

      <Part id="durations" kicker="Motion" title="Durations" wide text={<p>The complete set of timings. Use these values only.</p>}>
        <div className="overflow-x-auto rounded-[6px] border border-line">
          <table className="w-full min-w-[34rem] border-collapse text-left">
            <thead>
              <tr className="bg-raised">
                {["Movement", "Duration", "What happens"].map((h) => (
                  <th key={h} className="border-b border-line px-4 py-3 font-mono text-[10px] font-medium uppercase tracking-[0.18em] text-muted">
                    {h}
                  </th>
                ))}
              </tr>
            </thead>
            <tbody>
              {DURATIONS.map((d) => (
                <tr key={d.what}>
                  <td className="border-b border-line-soft px-4 py-3 font-mono text-[11.5px] uppercase tracking-[0.12em] text-fg">{d.what}</td>
                  <td className="border-b border-line-soft px-4 py-3 font-mono text-[11.5px] tabular-nums text-fg-soft">{d.value}</td>
                  <td className="border-b border-line-soft px-4 py-3 text-[12px] text-muted">{d.detail}</td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      </Part>

      <Part
        id="reveal"
        kicker="Motion"
        title="Word reveal"
        text={
          <p>
            Section titles rise into view word by word as they enter: 780 ms per word, 55 ms apart, on the
            Covenant curve. Use it for headlines only, never for body text.
          </p>
        }
      >
        <Panel tone="ground">
          <WordRevealDemo text="One computer, safely shared by humans and agents" />
        </Panel>
      </Part>

      <Part
        id="opening"
        kicker="Motion"
        title="Opening animation"
        text={
          <p>
            The home page opens on a grid of squares with the mark at its center. After half a second, the
            squares shrink, round off and clear diagonally from top left to bottom right, in the direction of
            the gradient. It plays once, on the home page only.
          </p>
        }
      >
        <Panel tone="ground">
          <CurtainDemo />
        </Panel>
      </Part>

      <Part
        id="forming"
        kicker="Motion"
        title="Forming the mark"
        text={
          <p>
            Covenant&apos;s signature motion. As you scroll the home page, the field comes together to form
            the mark. Because it follows the scroll, it moves at the reader&apos;s pace.
          </p>
        }
      >
        <Panel tone="ground" label="The final frame">
          <div className="flex aspect-[16/9] items-center justify-center">
            <CovenantMark className="h-[50%] w-auto" />
          </div>
        </Panel>
      </Part>

      <Part
        id="interaction"
        kicker="Motion"
        title="Interaction"
        text={
          <p>
            Buttons lift 2 px on hover, and the primary button also brightens. Links and cards change only
            their color or border. Focus is shown with a 2 px mint outline, offset 3 px.
          </p>
        }
      >
        <Panel tone="ground">
          <div className="flex flex-col gap-8 p-6 sm:p-8">
            <div className="flex flex-wrap gap-3">
              <button type="button" className="btn btn-primary">
                Try the sandbox <span aria-hidden>→</span>
              </button>
              <button type="button" className="btn btn-secondary">
                Read the docs <span aria-hidden>→</span>
              </button>
            </div>
            <div className="grid gap-3 sm:grid-cols-2">
              <a href="#interaction" className="rounded-[6px] border border-line bg-raised p-5 transition-colors hover:border-line-strong">
                <span className="font-mono text-[10px] uppercase tracking-[0.2em] text-muted-2">Product</span>
                <span className="type-display mt-2 block text-[1.2rem] text-fg">Covenant Guard</span>
              </a>
              <a href="#interaction" className="flex items-center rounded-[6px] border border-line p-5 font-mono text-[10.5px] font-medium uppercase tracking-[0.2em] text-fg transition-colors hover:text-mint">
                Explore Guard <span aria-hidden className="ml-2">→</span>
              </a>
            </div>
          </div>
        </Panel>
      </Part>

      <Part
        id="reduced"
        kicker="Motion"
        title="Reduced motion"
        text={<p>When a device requests reduced motion, Covenant removes animation without losing any content.</p>}
      >
        <Points
          items={[
            <>The opening animation is skipped.</>,
            <>Headlines and content appear in place; nothing rises or fades.</>,
            <>The home page shows the finished mark, and the bands between sections show a single still frame.</>,
            <>Pointer effects are hidden and smooth scrolling is turned off.</>,
          ]}
        />
      </Part>

      <Part id="misuse" kicker="Motion" title="Misuse" wide>
        <RuleGrid>
          <Rule ok={false} caption="Bounce, spring or overshoot">
            <Curve p1={[0.68, -0.6]} p2={[0.32, 1.6]} stroke="#e18383" className="h-[78%] w-auto" />
          </Rule>
          <Rule ok={false} caption="Spin or flip the mark">
            <span className="flex h-[42%] items-center [perspective:400px]">
              <span className="flex h-full [transform:rotateY(52deg)_rotateZ(-12deg)]">
                <CovenantMark className="h-full w-auto" />
              </span>
            </span>
          </Rule>
          <Rule ok={false} caption="Use motion blur or trails">
            <span className="relative flex h-[42%] items-center">
              {[0.12, 0.22, 0.4].map((o, i) => (
                <span key={o} className="absolute flex h-full" style={{ opacity: o, transform: `translateX(${-36 + i * 12}px)` }}>
                  <CovenantMark className="h-full w-auto" />
                </span>
              ))}
              <span className="relative flex h-full">
                <CovenantMark className="h-full w-auto" />
              </span>
            </span>
          </Rule>
          <Rule ok={false} caption="Loop for attention">
            <span className="btn btn-primary pointer-events-none motion-safe:animate-pulse">Buy now</span>
          </Rule>
        </RuleGrid>
      </Part>

      <MoreSections slug="motion" />
    </article>
  );
}
