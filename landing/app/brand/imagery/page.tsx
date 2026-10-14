import type { Metadata } from "next";
import { CovenantMark, MARK_PATHS, RAMP } from "../../_brand";
import { FieldSeam } from "../../FieldSeam";
import { ModuleField } from "../../ModuleField";
import { ResolveDemo } from "../_demos";
import { MoreSections, PageHead, Panel, Part, Rule, RuleGrid, Spec } from "../_kit";
import { moduleShape, rampAt, StaticField } from "../_visuals";

const DESCRIPTION =
  "Covenant's imagery: a moving field built from the shapes of the mark. It forms the logo, separates sections on the website and carries launches and announcements.";

export const metadata: Metadata = {
  title: "Imagery: Covenant brand guidelines",
  description: DESCRIPTION,
  alternates: { canonical: "/brand/imagery" },
  openGraph: {
    type: "website",
    url: "https://opencovenant.org/brand/imagery",
    title: "Imagery · Covenant brand guidelines",
    description: DESCRIPTION,
    images: [{ url: "/opengraph-image.jpg", width: 1200, height: 630 }],
  },
};

const canvasFill = "[&>canvas]:block [&>canvas]:h-full [&>canvas]:w-full";

/** The mark → one of its squares → the field made of that square. */
function ModuleOrigin() {
  return (
    <svg viewBox="0 0 1500 560" className="h-auto w-full" role="img" aria-label="One square taken from the mark, repeated to form the field">
      <defs>
        <linearGradient id="origin-ramp" x1="0" y1="0" x2="840" y2="1050" gradientUnits="userSpaceOnUse">
          <stop offset="0" stopColor={RAMP[0]} />
          <stop offset="0.5" stopColor={RAMP[1]} />
          <stop offset="1" stopColor={RAMP[2]} />
        </linearGradient>
      </defs>
      <g transform="translate(40 40) scale(0.457)">
        <g fill="url(#origin-ramp)" opacity="0.35">
          {MARK_PATHS.map((d) => (
            <path key={d} d={d} />
          ))}
        </g>
        <rect x="0" y="420" width="210" height="210" rx="38" fill="url(#origin-ramp)" />
        <rect x="-12" y="408" width="234" height="234" rx="46" fill="none" stroke="#f3f1fa" strokeWidth="6" strokeDasharray="10 12" />
      </g>
      <path d="M470 280 h120 m-22 -16 l22 16 l-22 16" fill="none" stroke="#8079a0" strokeWidth="3" />
      <rect x="640" y="200" width="160" height="160" rx="29" fill="#4c7fde" />
      <path d="M850 280 h120 m-22 -16 l22 16 l-22 16" fill="none" stroke="#8079a0" strokeWidth="3" />
      {Array.from({ length: 7 * 7 }, (_, i) => {
        const col = i % 7;
        const row = Math.floor(i / 7);
        const amount = Math.max(0, 1 - Math.abs(row - 3 - Math.sin(col * 0.9) * 1.4) / 2.6);
        if (amount < 0.05) return null;
        const { size, radius } = moduleShape(64, amount);
        const [r, g, b] = rampAt(Math.min(1, col / 6));
        return (
          <rect
            key={i}
            x={1020 + col * 64 + 32 - size / 2}
            y={56 + row * 64 + 32 - size / 2}
            width={size}
            height={size}
            rx={radius}
            fill={`rgb(${r} ${g} ${b})`}
            fillOpacity={Math.min(1, 0.38 + 0.8 * amount)}
          />
        );
      })}
      <text x="250" y="545" textAnchor="middle" fill="#8079a0" fontFamily="var(--font-jetbrains), monospace" fontSize="24" letterSpacing="3">
        THE MARK
      </text>
      <text x="720" y="545" textAnchor="middle" fill="#8079a0" fontFamily="var(--font-jetbrains), monospace" fontSize="24" letterSpacing="3">
        ONE SHAPE
      </text>
      <text x="1244" y="545" textAnchor="middle" fill="#8079a0" fontFamily="var(--font-jetbrains), monospace" fontSize="24" letterSpacing="3">
        THE FIELD
      </text>
    </svg>
  );
}

/** Shapes from faint to full: they grow, and their corners sharpen as they do. */
function DensityScale() {
  // The field's range, then the full shape as it appears in the mark.
  const steps = [0.06, 0.14, 0.24, 0.34, 0.42, 0.5, 1];
  return (
    <div className="grid grid-cols-7 gap-2">
      {steps.map((a) => {
        const { size, radius } = moduleShape(56, a);
        const [r, g, b] = rampAt(0.5);
        return (
          <div key={a} className="flex flex-col items-center gap-3">
            <svg viewBox="0 0 64 64" className="h-auto w-full max-w-[64px]" aria-hidden>
              <rect x={32 - size / 2} y={32 - size / 2} width={size} height={size} rx={radius} fill={`rgb(${r} ${g} ${b})`} fillOpacity={Math.min(1, 0.38 + 0.8 * a)} />
            </svg>
            <span className="font-mono text-[10px] tabular-nums text-muted-2">{a === 1 ? "Mark" : `${Math.round((0.2 + 0.84 * a) * 100)}%`}</span>
          </div>
        );
      })}
    </div>
  );
}

export default function BrandImagery() {
  return (
    <article>
      <PageHead
        slug="imagery"
        intro={
          <>
            <p>
              The identity extends beyond the logo. The shapes of the mark become a moving field, giving
              Covenant imagery of its own.
            </p>
            <p>
              The field opens our website, comes together to form the mark as you scroll and separates one
              section from the next. It is the only imagery Covenant uses, in place of photography and
              illustration.
            </p>
          </>
        }
      >
        <Panel tone="ground">
          <ModuleField className={`aspect-[16/10] w-full sm:aspect-[16/7] ${canvasFill}`} clearLeft={false} />
        </Panel>
      </PageHead>

      <Part
        id="origin"
        kicker="Imagery"
        title="Built from the mark"
        text={
          <p>
            The mark is built from a single shape, the rounded square. Repeated across a grid, that square
            becomes the field, so the logo and the imagery share one shape.
          </p>
        }
      >
        <Panel tone="ground">
          <div className="px-4 py-6 sm:px-8">
            <ModuleOrigin />
          </div>
        </Panel>
      </Part>

      <Part
        id="field"
        kicker="Imagery"
        title="The field"
        text={
          <>
            <p>
              A loose scatter of squares drifts slowly across the frame. Each shape grows and fades at its
              own pace, so the field always feels alive and never closes into a solid block.
            </p>
            <p>It responds to the reader: shapes grow beneath the pointer and move with the scroll.</p>
          </>
        }
      >
        <Spec
          rows={[
            ["Shape", "The rounded square of the mark"],
            ["Color", "The Covenant gradient, violet to mint"],
            ["Background", "Always the Covenant background, #0D0B16"],
            ["Density", "Sparse; shapes never touch"],
            ["Motion", "A slow, continuous drift that responds to scroll and pointer"],
            ["Where", "Website, share cards, social headers, launches and announcements"],
          ]}
        />
      </Part>

      <Part
        id="forming"
        kicker="Imagery"
        title="Forming the mark"
        text={
          <>
            <p>As you scroll the home page, the field comes together to form the Covenant mark.</p>
            <p>This is the field&apos;s only transition, and it always ends on the mark.</p>
          </>
        }
      >
        <Panel tone="ground">
          <ResolveDemo mode="scrub" className="aspect-[16/11] w-full" />
        </Panel>
      </Part>

      <Part
        id="transitions"
        kicker="Imagery"
        title="Between sections"
        text={
          <>
            <p>
              Sections are separated by a narrow band of the field rather than a rule. Its shapes move with
              the scroll and respond to the pointer, and the band stays sparse, so it never reads as a solid
              divider.
            </p>
            <p>
              Each band is 104–168 px tall and runs the full width of the page: one between each pair of
              sections and one above the footer.
            </p>
          </>
        }
      >
        <Panel tone="ground">
          <div className="px-6 pt-8 sm:px-8">
            <p className="font-mono text-[10px] uppercase tracking-[0.24em] text-muted">What you plug in</p>
            <p className="type-display mt-3 text-[1.5rem] text-fg">Guard &amp; trading</p>
          </div>
          <FieldSeam seed={1.3} className="my-2" />
          <div className="px-6 pb-8 sm:px-8">
            <p className="font-mono text-[10px] uppercase tracking-[0.24em] text-muted">Built in the open</p>
            <p className="type-display mt-3 text-[1.5rem] text-fg">
              The infrastructure <span className="text-ramp">builds itself</span>
            </p>
          </div>
        </Panel>
      </Part>

      <Part
        id="shape-size"
        kicker="Imagery"
        title="Shape size"
        text={
          <p>
            The larger a shape, the stronger it reads. The faintest shapes are small round dots; as they
            grow, their corners sharpen toward the corners of the mark. Across the field, shapes stay below two
            thirds of their grid cell and never touch one another. Only in the mark do they reach full size
            and join.
          </p>
        }
      >
        <Panel tone="ground" label="Size as a share of the grid cell">
          <div className="px-5 py-8 sm:px-8">
            <DensityScale />
          </div>
        </Panel>
      </Part>

      <Part
        id="color"
        kicker="Imagery"
        title="Color in the field"
        text={
          <p>
            The field carries the Covenant gradient across the frame, from violet at the top left to mint at
            the bottom right. In the bands between sections, the gradient runs straight across.
          </p>
        }
      >
        <Panel tone="ground">
          <div className="relative aspect-[16/9] w-full">
            <StaticField cols={48} rows={27} time={2.4} className="absolute inset-0 h-full w-full" />
            <span className="absolute left-4 top-4 font-mono text-[10px] uppercase tracking-[0.18em] text-violet">Violet</span>
            <span className="absolute bottom-4 right-4 font-mono text-[10px] uppercase tracking-[0.18em] text-mint">Mint</span>
          </div>
        </Panel>
      </Part>

      <Part id="misuse" kicker="Imagery" title="Misuse" wide>
        <RuleGrid>
          <Rule ok={false} caption="Run reading text over the field">
            <StaticField cols={24} rows={18} time={1.2} className="absolute inset-0 h-full w-full" />
            <p className="relative max-w-[80%] text-[11px] leading-relaxed text-fg">
              Every agent runs under a signed grant, and every action leaves a receipt in a hash-chained audit log.
            </p>
          </Rule>
          <Rule ok={false} caption="Recolor the field">
            <StaticField cols={24} rows={18} time={2.4} className="absolute inset-0 h-full w-full [filter:hue-rotate(150deg)_saturate(1.4)]" />
          </Rule>
          <Rule ok={false} caption="Replace the square with another shape">
            <svg viewBox="0 0 240 180" className="absolute inset-0 h-full w-full" aria-hidden>
              {Array.from({ length: 24 * 18 }, (_, i) => {
                const col = i % 24;
                const row = Math.floor(i / 24);
                const a = Math.max(0, 1 - Math.abs(row - 9 - Math.sin(col * 0.4) * 3) / 4);
                if (a < 0.08) return null;
                const [r, g, b] = rampAt(col / 23);
                return <circle key={i} cx={col * 10 + 5} cy={row * 10 + 5} r={1 + 4 * a} fill={`rgb(${r} ${g} ${b})`} />;
              })}
            </svg>
          </Rule>
          <Rule ok={false} caption="Blur it or add glow">
            <StaticField cols={24} rows={18} time={3.3} className="absolute inset-0 h-full w-full [filter:blur(3px)_brightness(1.4)]" />
          </Rule>
          <Rule ok={false} caption="Place the mark over it">
            <StaticField cols={24} rows={18} time={0.6} className="absolute inset-0 h-full w-full" />
            <CovenantMark className="relative h-[42%] w-auto" />
          </Rule>
          <Rule ok={false} caption="Tile it as a pattern">
            <span className="absolute inset-0 grid grid-cols-4 grid-rows-3">
              {Array.from({ length: 12 }, (_, i) => (
                <StaticField key={i} cols={8} rows={6} time={2.4} className="h-full w-full" />
              ))}
            </span>
          </Rule>
          <Rule ok={false} caption="Rotate or skew it">
            <StaticField cols={24} rows={18} time={2.4} className="absolute inset-[-20%] h-[140%] w-[140%] rotate-[28deg]" />
          </Rule>
          <Rule ok={false} caption="Place it on another background" className="bg-[#2b1150]">
            <StaticField cols={24} rows={18} time={4.1} className="absolute inset-0 h-full w-full" />
          </Rule>
        </RuleGrid>
      </Part>

      <MoreSections slug="imagery" />
    </article>
  );
}
