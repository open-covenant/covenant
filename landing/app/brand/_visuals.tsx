// Diagrams and renders for the brand guidelines, drawn from the same geometry
// as the mark and the same functions as the field shader, so every picture on
// these pages is the real thing rather than an illustration of it.

import type { ReactNode } from "react";
import { CovenantMark, MARK_PATHS, RAMP } from "../_brand";

// ── The field, frozen ───────────────────────────────────────────────────────
// The hero shader's ribbons evaluated on a grid at one instant: each cell is a
// rounded module sized by the ribbon under it, coloured along the ramp.

const VIOLET = [160, 59, 240];
const BLUE = [76, 127, 222];
const MINT = [29, 227, 174];

const clamp = (v: number, lo = 0, hi = 1) => Math.min(Math.max(v, lo), hi);
const smoothstep = (a: number, b: number, x: number) => {
  const t = clamp((x - a) / (b - a));
  return t * t * (3 - 2 * t);
};

function ribbon(x: number, y: number, offset: number, width: number, phase: number) {
  const c = 0.55 + 0.2 * Math.sin(x * 2.15 + phase) + 0.045 * Math.sin(x * 7 - phase * 0.7);
  const d = Math.abs(y - c - offset);
  return Math.exp(-(d * d) / width);
}

function ribbons(x: number, y: number, t: number) {
  const r1 = ribbon(x, y, 0.03, 0.0075, t + 0.9);
  const r2 = ribbon(x, y, -0.23, 0.0095, t + 3.25);
  const r3 = ribbon(x, y, 0.25, 0.016, t + 1.85);
  return clamp(r1 * 1.15 + r2 * 1.05 + r3 * 0.6, 0, 1.4);
}

export function rampAt(g: number) {
  const [a, b, t] = g < 0.5 ? [VIOLET, BLUE, g * 2] : [BLUE, MINT, (g - 0.5) * 2];
  return a.map((v, i) => Math.round(v + (b[i] - v) * t));
}

/** One module at a given intensity, as the field draws it. */
export function moduleShape(cell: number, amount: number) {
  const half = cell * (0.1 + 0.42 * amount);
  return { size: half * 2, radius: half * (0.92 - 0.78 * amount) };
}

// A deterministic per-cell hash, standing in for the shader's.
function cellHash(i: number, j: number) {
  const v = Math.sin(i * 12.9898 + j * 78.233) * 43758.5453;
  return v - Math.floor(v);
}

export function StaticField({
  cols = 48,
  rows = 27,
  time = 2.4,
  clearLeft = false,
  strip = false,
  className = "",
}: {
  cols?: number;
  rows?: number;
  time?: number;
  clearLeft?: boolean;
  /** Draw it as a seam: a sparse slice of the field that thins out top and bottom. */
  strip?: boolean;
  className?: string;
}) {
  const cell = 10;
  const cells: ReactNode[] = [];
  for (let j = 0; j < rows; j++) {
    for (let i = 0; i < cols; i++) {
      const x = (i + 0.5) / cols;
      const yDown = (j + 0.5) / rows;
      let amount: number;
      let strength: number;
      if (strip) {
        // As FieldSeam draws it: a cell lights only when enough ribbon passes
        // under it, breathes on its own clock, and is capped short of its cell.
        const env = smoothstep(0, 0.5, yDown) * smoothstep(0, 0.5, 1 - yDown);
        const e = ribbons(x, 0.55 + (0.5 - yDown) * 0.22, time * 0.34) * 0.7 * env;
        const h = cellHash(i, j);
        const gate = smoothstep(0.1 + h * 0.9, 0.3 + h * 0.9, e);
        const breathe = 0.5 + 0.5 * Math.sin(time * (0.8 + h * 1.6) + h * 6.2831);
        amount = Math.min(e * gate * (0.4 + 0.6 * breathe), 0.5);
        const fade = smoothstep(0, 0.3, yDown) * smoothstep(0, 0.3, 1 - yDown);
        strength = (0.35 + 0.65 * (amount / 0.5)) * fade;
      } else {
        // As ModuleField draws its field: gated per cell, breathing, capped.
        let e = ribbons(x, 1 - yDown, time * 0.3) * 0.8;
        if (clearLeft) e *= smoothstep(0.22, 0.68, x);
        const h = cellHash(i, j);
        const gate = smoothstep(0.1 + h * 0.9, 0.3 + h * 0.9, e);
        const breathe = 0.5 + 0.5 * Math.sin(time * (0.8 + h * 1.6) + h * 6.2831);
        amount = Math.min(e * gate * (0.4 + 0.6 * breathe), 0.5);
        strength = 0.35 + 0.65 * (amount / 0.5);
      }
      if (amount < 0.02) continue;
      const { size, radius } = moduleShape(cell, amount);
      const [cr, cg, cb] = rampAt(strip ? x : clamp(x * 0.62 + yDown * 0.38));
      cells.push(
        <rect
          key={`${i}-${j}`}
          x={(i + 0.5) * cell - size / 2}
          y={(j + 0.5) * cell - size / 2}
          width={size}
          height={size}
          rx={radius}
          fill={`rgb(${cr} ${cg} ${cb})`}
          fillOpacity={strength}
        />,
      );
    }
  }
  return (
    <svg viewBox={`0 0 ${cols * cell} ${rows * cell}`} preserveAspectRatio="xMidYMid slice" className={className} aria-hidden>
      {cells}
    </svg>
  );
}

// ── The mark, drawn for diagrams ────────────────────────────────────────────

function RampDef({ id }: { id: string }) {
  return (
    <linearGradient id={id} x1="0" y1="0" x2="840" y2="1050" gradientUnits="userSpaceOnUse">
      <stop offset="0" stopColor={RAMP[0]} />
      <stop offset="0.5" stopColor={RAMP[1]} />
      <stop offset="1" stopColor={RAMP[2]} />
    </linearGradient>
  );
}

const DIM = "#8079a0";
const LINE = "#3d3757";
const TEXT = { fontFamily: "var(--font-jetbrains), monospace", fontSize: 30, letterSpacing: "0.12em", fill: DIM } as const;

/** The mark on its 4 × 5 grid of 210-unit modules, with its radii called out. */
export function MarkConstruction({ className = "" }: { className?: string }) {
  return (
    <svg viewBox="-190 -170 1220 1400" className={className} role="img" aria-label="The mark on its four by five grid of 210-unit modules">
      <defs>
        <RampDef id="construction-ramp" />
      </defs>
      {Array.from({ length: 5 }, (_, i) => (
        <line key={`v${i}`} x1={i * 210} y1={-40} x2={i * 210} y2={1090} stroke={LINE} strokeWidth="2" strokeDasharray="6 10" />
      ))}
      {Array.from({ length: 6 }, (_, j) => (
        <line key={`h${j}`} x1={-40} y1={j * 210} x2={880} y2={j * 210} stroke={LINE} strokeWidth="2" strokeDasharray="6 10" />
      ))}
      <g fill="url(#construction-ramp)">
        {MARK_PATHS.map((d) => (
          <path key={d} d={d} />
        ))}
      </g>
      <rect x="0" y="420" width="210" height="210" fill="none" stroke="#f3f1fa" strokeWidth="3" />
      <text x="105" y="532" textAnchor="middle" {...TEXT} fill="#f3f1fa">
        210
      </text>
      {/* overall dimensions */}
      <g stroke={DIM} strokeWidth="2">
        <line x1="0" y1="-90" x2="840" y2="-90" />
        <line x1="0" y1="-108" x2="0" y2="-72" />
        <line x1="840" y1="-108" x2="840" y2="-72" />
        <line x1="-110" y1="0" x2="-110" y2="1050" />
        <line x1="-128" y1="0" x2="-92" y2="0" />
        <line x1="-128" y1="1050" x2="-92" y2="1050" />
      </g>
      <text x="420" y="-112" textAnchor="middle" {...TEXT}>
        840 · 4 MODULES
      </text>
      <text x="-130" y="525" textAnchor="middle" transform="rotate(-90 -130 525)" {...TEXT}>
        1050 · 5 MODULES
      </text>
      {/* radii */}
      <circle cx="802" cy="38" r="38" fill="none" stroke="#f3f1fa" strokeWidth="2" strokeDasharray="4 6" />
      <text x="860" y="30" {...TEXT}>
        R 38
      </text>
      <circle cx="618" cy="432" r="47" fill="none" stroke="#f3f1fa" strokeWidth="2" strokeDasharray="4 6" />
      <text x="690" y="410" {...TEXT}>
        R 47
      </text>
      <text x="420" y="1170" textAnchor="middle" {...TEXT}>
        CORE 280 × 280
      </text>
    </svg>
  );
}

/** The detached core, and the gap that keeps it from the C. */
export function CoreDiagram({ className = "" }: { className?: string }) {
  return (
    <svg viewBox="-120 -80 1080 1210" className={className} role="img" aria-label="The core sits inside the C without touching it">
      <defs>
        <RampDef id="core-ramp" />
      </defs>
      <path d={MARK_PATHS[0]} fill="none" stroke={LINE} strokeWidth="4" />
      <path d={MARK_PATHS[1]} fill="url(#core-ramp)" />
      <g stroke={DIM} strokeWidth="2">
        <line x1="210" y1="525" x2="385" y2="525" strokeDasharray="5 7" />
        <line x1="525" y1="210" x2="525" y2="385" strokeDasharray="5 7" />
        <line x1="525" y1="665" x2="525" y2="840" strokeDasharray="5 7" />
      </g>
      <text x="297" y="505" textAnchor="middle" {...TEXT}>
        175
      </text>
      <text x="548" y="306" {...TEXT}>
        175
      </text>
      <text x="548" y="764" {...TEXT}>
        175
      </text>
    </svg>
  );
}

/** Clear space: one module, a fifth of the mark's height, on every side. */
export function ClearSpace({ className = "" }: { className?: string }) {
  const corners: [number, number][] = [
    [-210, -210],
    [840, -210],
    [-210, 1050],
    [840, 1050],
  ];
  return (
    <svg viewBox="-300 -300 1440 1650" className={className} role="img" aria-label="Clear space of one module around the mark">
      <defs>
        <RampDef id="clear-ramp" />
      </defs>
      <rect x="-210" y="-210" width="1260" height="1470" fill="none" stroke={DIM} strokeWidth="3" strokeDasharray="10 12" />
      {corners.map(([x, y]) => (
        <rect key={`${x}${y}`} x={x} y={y} width="210" height="210" rx="38" fill="#2a2640" />
      ))}
      <g fill="url(#clear-ramp)">
        {MARK_PATHS.map((d) => (
          <path key={d} d={d} />
        ))}
      </g>
      <text x="-105" y="-235" textAnchor="middle" {...TEXT}>
        1 MODULE
      </text>
    </svg>
  );
}

// ── Mockups ─────────────────────────────────────────────────────────────────

/** A dark browser tab carrying the favicon. */
export function BrowserTab({ title = "Covenant" }: { title?: string }) {
  return (
    <div className="w-full max-w-[30rem] overflow-hidden rounded-[8px] border border-line bg-sunken">
      <div className="flex items-end gap-2 px-3 pt-2.5">
        <span className="mb-2.5 flex gap-1.5" aria-hidden>
          <span className="h-2.5 w-2.5 rounded-full bg-line-strong" />
          <span className="h-2.5 w-2.5 rounded-full bg-line-strong" />
          <span className="h-2.5 w-2.5 rounded-full bg-line-strong" />
        </span>
        <span className="ml-2 flex min-w-0 items-center gap-2 rounded-t-[7px] bg-raised px-3 py-2 font-mono text-[11px] text-fg">
          <CovenantMark className="h-3.5 w-auto shrink-0" />
          <span className="truncate">{title}</span>
          <span className="ml-3 text-muted-2" aria-hidden>
            ×
          </span>
        </span>
      </div>
      <div className="flex items-center gap-3 bg-raised px-3 py-2.5">
        <span className="font-mono text-[12px] text-muted-2" aria-hidden>
          ← →
        </span>
        <span className="flex-1 rounded-full bg-ground px-3 py-1.5 font-mono text-[11px] text-muted">opencovenant.org</span>
      </div>
    </div>
  );
}

/** The app icon: the ramp mark on night, at the proportions the icon files use. */
export function AppIcon({
  size = 112,
  shape = "squircle",
  label = "Covenant app icon",
}: {
  size?: number;
  shape?: "squircle" | "circle";
  label?: string;
}) {
  // 69% of the height in a square icon (the apple-icon ratio), 50% in a circle.
  const markH = Math.round(size * (shape === "circle" ? 0.5 : 0.69));
  return (
    <span
      role="img"
      aria-label={label}
      className={`flex shrink-0 items-center justify-center border border-line bg-ground ${shape === "circle" ? "rounded-full" : "rounded-[22%]"}`}
      style={{ width: size, height: size }}
    >
      <span className="flex" style={{ height: markH }}>
        <CovenantMark className="h-full w-auto" />
      </span>
    </span>
  );
}

/** The name, set as the lockup sets it: JetBrains Mono Medium, 0.24em. */
function Name({ children, className = "" }: { children: ReactNode; className?: string }) {
  return <span className={`font-mono font-medium uppercase leading-none tracking-[0.24em] ${className}`}>{children}</span>;
}

/**
 * A sub-brand lockup: the mark, the name, a hairline and the descriptor, all at
 * the header's proportions (a 30px mark to an 11px name, 10px apart). `fontSize`
 * scales the whole lockup.
 */
export function SubBrandLockup({
  descriptor,
  fontSize = 11,
  withName = true,
}: {
  descriptor: string;
  fontSize?: number;
  withName?: boolean;
}) {
  return (
    <span className="inline-flex items-center text-fg" style={{ fontSize, gap: "0.91em" }}>
      <CovenantMark className="h-[2.73em] w-auto shrink-0" />
      {withName ? <Name>Covenant</Name> : null}
      {withName ? <span aria-hidden className="h-[1.5em] w-px shrink-0 bg-line-strong" /> : null}
      <Name className="text-muted">{descriptor}</Name>
    </span>
  );
}

/** The primary lockup at any size, built like the outlined file. */
export function Lockup({ fontSize = 11, className = "" }: { fontSize?: number; className?: string }) {
  return (
    <span className={`inline-flex items-center text-fg ${className}`} style={{ fontSize, gap: "0.91em" }}>
      <CovenantMark className="h-[2.73em] w-auto shrink-0" />
      <Name>Covenant</Name>
    </span>
  );
}

/** A stand-in for a partner's mark: never a real company's logo. */
export function PartnerMark({ fontSize = 11 }: { fontSize?: number }) {
  return (
    <span className="inline-flex items-center text-fg-2" style={{ fontSize, gap: "0.7em" }}>
      <span aria-hidden className="flex h-[2.73em] w-[2.73em] shrink-0 items-center justify-center rounded-full border-[0.18em] border-current">
        <span className="h-[0.9em] w-[0.9em] rounded-full bg-current" />
      </span>
      <span className="font-sans text-[1.35em] font-semibold leading-none tracking-[-0.01em]">Partner</span>
    </span>
  );
}

/** Covenant and a partner, side by side across a multiplication sign. */
export function PartnerLockup({ fontSize = 11, partnerFirst = false }: { fontSize?: number; partnerFirst?: boolean }) {
  const covenant = <Lockup fontSize={fontSize} />;
  const partner = <PartnerMark fontSize={fontSize} />;
  return (
    <span className="inline-flex items-center" style={{ fontSize, gap: "1.9em" }}>
      {partnerFirst ? partner : covenant}
      <span aria-hidden className="font-mono text-[1.6em] font-light leading-none text-muted-2">
        ×
      </span>
      {partnerFirst ? covenant : partner}
    </span>
  );
}
