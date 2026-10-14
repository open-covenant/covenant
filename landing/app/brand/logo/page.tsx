import type { Metadata } from "next";
import Image from "next/image";
import { CovenantMark, MARK_PATHS, RAMP } from "../../_brand";
import { CopyFileButton } from "../CopyButton";
import { DownloadLink, MoreSections, PageHead, Panel, Part, Rule, RuleGrid, Spec } from "../_kit";
import { AppIcon, BrowserTab, ClearSpace, CoreDiagram, Lockup, MarkConstruction } from "../_visuals";

const DESCRIPTION =
  "The Covenant logo: the mark and the lockup, their construction, the core, color versions, clear space, minimum sizes, icons, misuse and files.";

export const metadata: Metadata = {
  title: "Logo: Covenant brand guidelines",
  description: DESCRIPTION,
  alternates: { canonical: "/brand/logo" },
  openGraph: {
    type: "website",
    url: "https://opencovenant.org/brand/logo",
    title: "Logo · Covenant brand guidelines",
    description: DESCRIPTION,
    images: [{ url: "/opengraph-image.jpg", width: 1200, height: 630 }],
  },
};

// The mark with a fill of the example's choosing, for the misuse tiles.
function Mark({
  fill,
  stops,
  stroke,
  className = "h-[46%] w-auto",
  coreOffset,
  style,
}: {
  fill?: string;
  stops?: readonly string[];
  stroke?: boolean;
  className?: string;
  coreOffset?: [number, number];
  style?: React.CSSProperties;
}) {
  const id = `misuse-${(stops ?? [fill ?? "x"]).join("").replace(/[^\w]/g, "")}-${stroke ? "s" : "f"}`;
  const paint = stops ? `url(#${id})` : (fill ?? "#f3f1fa");
  return (
    <svg viewBox="-60 -60 960 1170" className={className} style={style} aria-hidden>
      {stops ? (
        <defs>
          <linearGradient id={id} x1="0" y1="0" x2="840" y2="1050" gradientUnits="userSpaceOnUse">
            {stops.map((c, i) => (
              <stop key={c} offset={i / (stops.length - 1)} stopColor={c} />
            ))}
          </linearGradient>
        </defs>
      ) : null}
      <path d={MARK_PATHS[0]} fill={stroke ? "none" : paint} stroke={stroke ? paint : undefined} strokeWidth={stroke ? 22 : undefined} />
      <path
        d={MARK_PATHS[1]}
        transform={coreOffset ? `translate(${coreOffset[0]} ${coreOffset[1]})` : undefined}
        fill={stroke ? "none" : paint}
        stroke={stroke ? paint : undefined}
        strokeWidth={stroke ? 22 : undefined}
      />
    </svg>
  );
}

const FILES = [
  { title: "Mark", note: "The mark in the Covenant gradient. The default.", href: "/covenant-mark.svg", file: "covenant-mark.svg", shape: "mark" },
  { title: "Mark, white", note: "One color, for single-color work.", href: "/covenant-mark-white.svg", file: "covenant-mark-white.svg", shape: "mark" },
  { title: "Lockup", note: "The mark and the name, with the name converted to outlines so no font is needed.", href: "/covenant-lockup.svg", file: "covenant-lockup.svg", shape: "lockup" },
  { title: "Lockup, white", note: "One color: the mark and the name in white.", href: "/covenant-lockup-white.svg", file: "covenant-lockup-white.svg", shape: "lockup" },
] as const;

export default function BrandLogo() {
  return (
    <article>
      <PageHead
        slug="logo"
        intro={
          <p>
            The Covenant mark is an independent core within a C: agents with room to act, inside clearly
            defined limits. It is drawn on a strict grid, carries the Covenant gradient and always appears
            on a dark background. The lockup pairs the mark with the name, set in JetBrains Mono.
          </p>
        }
      >
        <Panel tone="ground" actions={<CopyFileButton src="/covenant-mark.svg" label="Copy the mark as SVG" />}>
          <div className="flex aspect-[16/9] items-center justify-center sm:aspect-[16/7]">
            <CovenantMark className="h-[58%] w-auto" title="The Covenant mark" />
          </div>
        </Panel>
      </PageHead>

      <Part
        id="mark"
        kicker="Logo"
        title="The mark"
        text={
          <>
            <p>
              Three bars and a core. The stem joins the top and bottom bars to form the C, and the core sits
              at its center, separate from all three.
            </p>
            <p>
              The mark is built from rounded squares. The field, Covenant&apos;s moving imagery, uses the same
              squares, and on the home page they come together to form the mark.
            </p>
          </>
        }
      >
        <div className="grid gap-4 sm:grid-cols-3">
          {[
            { h: "h-28", tone: "ground" as const, label: "On the background" },
            { h: "h-16", tone: "raised" as const, label: "On a surface" },
            { h: "h-9", tone: "sunken" as const, label: "Small" },
          ].map((v) => (
            <Panel key={v.label} tone={v.tone} label={v.label}>
              <div className="flex aspect-square items-center justify-center">
                <CovenantMark className={`${v.h} w-auto`} />
              </div>
            </Panel>
          ))}
        </div>
      </Part>

      <Part
        id="construction"
        kicker="Logo"
        title="Construction"
        text={
          <>
            <p>
              The mark is drawn on a grid of 4 × 5 modules. Each module is 210 units square, so the mark
              measures 840 × 1050 overall, a 4:5 rectangle. Each bar is one module deep; the stem is one module
              wide and three tall.
            </p>
            <p>
              Outer corners have a radius of 38 units, just under a fifth of a module. Where the stem meets
              each bar, the same radius forms an inner curve. Always use the supplied files; never redraw the
              mark.
            </p>
          </>
        }
      >
        <Panel tone="ground" label="Construction" caption="Dashed: the 210-unit grid. Outlined: one module.">
          <div className="flex items-center justify-center px-6 py-8">
            <MarkConstruction className="h-auto w-full max-w-[26rem]" />
          </div>
        </Panel>
        <Spec
          className="mt-6"
          rows={[
            ["Grid", "4 × 5 modules · 210 units each"],
            ["Box", "840 × 1050 · 4:5"],
            ["Corners", "38-unit radius outside; the same radius inside, where the stem meets each bar"],
            ["Core", "280 × 280 · corner 47 · centered in the C"],
          ]}
        />
      </Part>

      <Part
        id="core"
        kicker="Logo"
        title="The core"
        text={
          <>
            <p>
              The core sits inside the C, 175 units from it on three sides, and never touches it. That space
              carries the idea: an agent should be able to do its job without being given unlimited control.
              It sits within the boundary, separate from it.
            </p>
            <p>Never let the core touch the C, move off center or change size.</p>
          </>
        }
      >
        <Panel tone="ground" label="Within the boundary">
          <div className="flex items-center justify-center px-6 py-8">
            <CoreDiagram className="h-auto w-full max-w-[22rem]" />
          </div>
        </Panel>
      </Part>

      <Part
        id="color"
        kicker="Logo"
        title="Color"
        text={
          <>
            <p>
              The mark carries the Covenant gradient: violet at the top left, blue through the middle and mint
              at the bottom right. This is the default version.
            </p>
            <p>
              Where a gradient is not possible (embossing, single-color print, monochrome interfaces), use the
              white version. Never place the mark on white or a light background.
            </p>
          </>
        }
      >
        <div className="grid gap-4 sm:grid-cols-2">
          <Panel tone="ground" label="Gradient · default">
            <div className="flex aspect-[4/3] items-center justify-center">
              <CovenantMark className="h-[52%] w-auto" />
            </div>
          </Panel>
          <Panel tone="ground" label="White · one color">
            <div className="flex aspect-[4/3] items-center justify-center text-white">
              <CovenantMark tone="mono" className="h-[52%] w-auto" />
            </div>
          </Panel>
        </div>
      </Part>

      <Part
        id="lockup"
        kicker="Logo"
        title="Lockup"
        text={
          <>
            <p>
              The mark and the name in JetBrains Mono Medium capitals. Use it wherever Covenant introduces
              itself: headers, title slides, partner material.
            </p>
            <p>Use the mark alone once the name is on screen, or where space is tight.</p>
          </>
        }
      >
        <Panel tone="ground" actions={<CopyFileButton src="/covenant-lockup.svg" label="Copy the lockup as SVG" />}>
          <div className="flex aspect-[16/7] items-center justify-center px-8">
            <Image src="/covenant-lockup.svg" alt="The Covenant lockup" width={3685} height={1050} className="h-auto w-[72%] max-w-[30rem]" />
          </div>
        </Panel>
      </Part>

      <Part
        id="lockup-construction"
        kicker="Logo"
        title="Lockup construction"
        text={
          <>
            <p>
              All measures derive from the mark&apos;s height, <strong>H</strong>. The gap to the name is H/3.
              The capitals are 0.27 H tall, centered on the mark and tracked at 0.24 em.
            </p>
            <p>At header size: a 30 px mark, an 11 px name, 10 px apart.</p>
          </>
        }
      >
        <Panel tone="ground" label="Measured from H">
          <div className="px-6 pb-10 pt-14 sm:px-10">
            <div className="relative mx-auto w-full max-w-[34rem]" style={{ aspectRatio: "3685 / 1050" }}>
              <Image src="/covenant-lockup.svg" alt="" width={3685} height={1050} className="absolute inset-0 h-full w-full" />
              {/* mark height */}
              <span aria-hidden className="absolute -left-4 top-0 h-full border-l border-dashed border-muted-2" />
              <span className="absolute -left-9 top-1/2 -translate-y-1/2 font-mono text-[10px] text-muted">H</span>
              {/* gap: 840 → 1190 of 3685 */}
              <span aria-hidden className="absolute top-0 h-full border-x border-dashed border-muted-2 bg-blue/10" style={{ left: "22.8%", width: "9.5%" }} />
              <span className="absolute -top-6 -translate-x-1/2 whitespace-nowrap font-mono text-[10px] text-muted" style={{ left: "27.5%" }}>
                H/3
              </span>
              {/* cap height band: 36.6% → 63.4% */}
              <span aria-hidden className="absolute right-0 border-y border-dashed border-muted-2" style={{ left: "32.3%", top: "36.6%", height: "26.8%" }} />
              <span className="absolute -bottom-7 whitespace-nowrap font-mono text-[10px] text-muted" style={{ left: "32.3%" }}>
                CAPS 0.27 H · TRACKING 0.24 EM
              </span>
            </div>
          </div>
        </Panel>
      </Part>

      <Part
        id="clear-space"
        kicker="Logo"
        title="Clear space"
        text={
          <>
            <p>
              Keep clear space of one module on every side, a fifth of the mark&apos;s height. For the
              lockup, measure from the mark.
            </p>
            <p>Keep type, edges and other logos out of this space.</p>
          </>
        }
      >
        <Panel tone="ground" label="One module">
          <div className="flex items-center justify-center px-6 py-8">
            <ClearSpace className="h-auto w-full max-w-[20rem]" />
          </div>
        </Panel>
      </Part>

      <Part
        id="sizes"
        kicker="Logo"
        title="Minimum sizes"
        text={
          <p>
            The mark remains legible down to 16 px, its favicon size. Below 20 px in an interface, use the
            mark without the name. The lockup needs at least 120 px of width.
          </p>
        }
      >
        <div className="grid gap-4 sm:grid-cols-2">
          <Panel tone="ground" label="Mark">
            <div className="flex h-40 items-end justify-center gap-8 pb-8">
              {[16, 20, 24, 32, 48].map((h) => (
                <span key={h} className="flex flex-col items-center gap-3">
                  <span style={{ height: h }} className="flex">
                    <CovenantMark className="h-full w-auto" />
                  </span>
                  <span className="font-mono text-[10px] tabular-nums text-muted-2">{h}</span>
                </span>
              ))}
            </div>
          </Panel>
          <Panel tone="ground" label="Lockup">
            <div className="flex h-40 flex-col items-start justify-center gap-4 px-6">
              {[120, 160, 220].map((w) => (
                <span key={w} className="flex items-center gap-4">
                  <Image src="/covenant-lockup.svg" alt="" width={3685} height={1050} style={{ width: w, height: "auto" }} />
                  <span className="font-mono text-[10px] tabular-nums text-muted-2">{w} px</span>
                </span>
              ))}
            </div>
          </Panel>
        </div>
      </Part>

      <Part
        id="icons"
        kicker="Logo"
        title="Favicon & app icons"
        text={
          <>
            <p>
              Favicon: the gradient mark on a transparent background at 16, 32 and 48 px. Browser tabs are the
              one place the mark may appear on a light background. App icon: the mark on
              the Covenant background at 69% of the icon&apos;s height. Maskable icon: 55%, so any mask keeps
              it whole. Avatar: 50%.
            </p>
            <p>No borders, background gradients or white backgrounds, whatever the platform default.</p>
          </>
        }
      >
        <div className="grid gap-4 sm:grid-cols-2">
          <Panel tone="ground" label="Favicon" outerClassName="sm:col-span-2">
            <div className="flex items-center justify-center p-6 sm:p-10">
              <BrowserTab />
            </div>
          </Panel>
          <Panel tone="raised" label="App icon">
            <div className="flex h-48 items-center justify-center">
              <AppIcon size={112} />
            </div>
          </Panel>
          <Panel tone="raised" label="Avatar">
            <div className="flex h-48 items-center justify-center">
              <AppIcon size={112} shape="circle" label="Covenant avatar" />
            </div>
          </Panel>
        </div>
      </Part>

      <Part id="misuse" kicker="Logo" title="Misuse" wide text={<p>Use the supplied files, unaltered.</p>}>
        <RuleGrid cols={3}>
          <Rule ok={false} caption="Recolor it">
            <Mark fill="#e0b968" />
          </Rule>
          <Rule ok={false} caption="Reverse or reorder the gradient">
            <Mark stops={[RAMP[2], RAMP[1], RAMP[0]]} />
          </Rule>
          <Rule ok={false} caption="Stretch or squash it">
            <Mark stops={RAMP} style={{ transform: "scaleX(1.45)" }} />
          </Rule>
          <Rule ok={false} caption="Rotate it">
            <Mark stops={RAMP} style={{ transform: "rotate(18deg)" }} />
          </Rule>
          <Rule ok={false} caption="Outline it">
            <Mark stops={RAMP} stroke />
          </Rule>
          <Rule ok={false} caption="Add glows or shadows">
            <Mark stops={RAMP} style={{ filter: "drop-shadow(0 0 18px rgb(160 59 240 / 0.9)) drop-shadow(0 10px 8px rgb(0 0 0 / 0.8))" }} />
          </Rule>
          <Rule ok={false} caption="Move or attach the core">
            <Mark stops={RAMP} coreOffset={[-175, 0]} />
          </Rule>
          <Rule ok={false} caption="Place it on the gradient or on photos" className="bg-ramp">
            <Mark stops={RAMP} />
          </Rule>
          <Rule ok={false} caption="Rebuild the lockup in another typeface">
            <span className="flex items-center gap-3">
              <CovenantMark className="h-10 w-auto" />
              <span className="font-serif text-[22px] italic text-fg">Covenant</span>
            </span>
          </Rule>
        </RuleGrid>
      </Part>

      <Part id="files" kicker="Logo" title="Files" text={<p>SVG for anything that scales; a 1024 px PNG where pixels are required. Every file is also in the brand assets zip.</p>}>
        <ul className="grid gap-3 sm:grid-cols-2">
          {FILES.map((f) => (
            <li key={f.file} className="flex flex-col overflow-hidden rounded-[6px] border border-line bg-raised">
              <div className="flex h-36 items-center justify-center border-b border-line bg-ground px-8">
                <Image
                  src={f.href}
                  alt=""
                  width={f.shape === "mark" ? 840 : 3685}
                  height={1050}
                  className={f.shape === "mark" ? "h-14 w-auto" : "h-9 w-auto"}
                />
              </div>
              <div className="flex flex-1 flex-col gap-3 p-4">
                <div className="flex items-center justify-between gap-3">
                  <span className="font-mono text-[12px] font-semibold uppercase tracking-[0.16em] text-fg">{f.title}</span>
                  <span className="font-mono text-[10px] uppercase tracking-[0.2em] text-muted-2">SVG</span>
                </div>
                <p className="text-[12px] leading-relaxed text-muted">{f.note}</p>
                <div className="mt-auto flex flex-wrap gap-2">
                  <DownloadLink href={f.href} file={f.file}>
                    Download
                  </DownloadLink>
                  <CopyFileButton src={f.href} label={`Copy ${f.title} as SVG`} />
                </div>
              </div>
            </li>
          ))}
          <li className="flex flex-col justify-between gap-4 rounded-[6px] border border-line bg-raised p-4 sm:col-span-2 sm:flex-row sm:items-center">
            <span className="flex items-center gap-4">
              <Lockup fontSize={9} />
              <span className="text-[12px] leading-relaxed text-muted">The gradient mark as a 1024 px transparent PNG.</span>
            </span>
            <DownloadLink href="/covenant-mark.png" file="covenant-mark.png">
              PNG
            </DownloadLink>
          </li>
        </ul>
      </Part>

      <MoreSections slug="logo" />
    </article>
  );
}
