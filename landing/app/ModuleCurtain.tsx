"use client";

import { useEffect, useLayoutEffect, useRef, useState } from "react";
import { CovenantMark } from "./_brand";
import { createFieldProgram, FIELD_GLSL, fieldCellSize, MARK_GLSL } from "./_field";

// The opening: the page starts behind a solid sheet of the field's modules
// with the mark at its centre. The mark comes apart into its modules as the
// sheet breaks up cell by cell, top left to bottom right along the ramp. Each
// module shrinks in place and lights as it goes; those on the ribbons land on
// the hero's modules and fade into them, the rest clear to the page. Nothing
// slides: modules only grow, shrink, fuse and light, as everywhere else.
//
// The server renders a plain cover (ground and mark) so the first paint is
// already hidden; the canvas draws the same picture and takes over on its first
// frame. With JS off a <noscript> style drops the cover; if JS is on but never
// runs, a CSS animation lets the page through after 4 s.

const FRAG = `
precision highp float;
uniform float time;
uniform float fieldTime;
uniform float cellSize;
uniform vec2 markOrigin;
uniform float markUnit;
uniform vec2 fieldOrigin;
uniform vec2 fieldSize;
uniform float clearing;
uniform vec3 veilTop;
uniform vec3 veilBottom;
uniform float fieldOpacity;

${FIELD_GLSL}

${MARK_GLSL}

float hash(vec2 p) {
  p = fract(p * vec2(123.34, 456.21));
  p += dot(p, p + 45.32);
  return fract(p.x * p.y);
}

vec2 toMark(vec2 px) {
  vec2 m = (px - markOrigin) / markUnit;
  return vec2(m.x, 5.0 - m.y);
}

// Position along the ramp, violet at the top left to mint at the bottom right.
float diagonal(vec2 uv) {
  return clamp(uv.x * 0.62 + (1.0 - uv.y) * 0.38, 0.0, 1.0);
}

// The Covenant curve, near enough: a quick start and a long, soft landing.
float easeOut(float x) {
  x = clamp(x, 0.0, 1.0);
  return 1.0 - pow(1.0 - x, 5.0);
}

float easeInOut(float x) {
  x = clamp(x, 0.0, 1.0);
  return x < 0.5 ? 4.0 * x * x * x : 1.0 - pow(-2.0 * x + 2.0, 3.0) / 2.0;
}

// Timeline, in seconds. The sheet holds, then each cell breaks at its own
// moment: later towards the bottom right, jittered per cell. The mark starts a
// little before the sheet, since its first stretch barely changes it, so the
// two visibly begin together.
const float HOLD = 0.5;
const float MARK_LEAD = 0.2;
const float MARK_TIME = 0.85;
const float SPREAD = 0.55;
const float JITTER = 0.2;
const float BREAK = 0.6;
const float LINGER = 0.2;
const float FADE = 0.4;

// As in the hero: the largest a field module grows, and the ribbons' resting
// drift before the pointer has moved.
const float CAP = 0.5;
const float DRIFT = 0.0132;
// How brightly a cell off the ribbons lights on its way out.
const float FLARE = 0.38;

// The hero's field under a cell at rest (no scroll, no pointer), on
// ModuleField's clock, so the modules that stay match the hero's.
float heroField(vec2 center, float h) {
  vec2 uv = center / fieldSize;
  float clear = mix(1.0, smoothstep(0.22, 0.68, uv.x), clearing);
  float e = ribbons(uv, DRIFT, fieldTime * 0.3) * 0.8 * clear;
  float gate = smoothstep(0.1 + h * 0.9, 0.3 + h * 0.9, e);
  float breathe = 0.5 + 0.5 * sin(fieldTime * (0.8 + h * 1.6) + h * 6.2831);
  return min(e * gate * mix(0.4, 1.0, breathe), CAP);
}

// How much of a hero module shows through the hero's opacity and veil.
float throughVeil(vec2 uv) {
  float top = mix(veilTop.x, veilTop.y, clamp((1.0 - uv.y) / veilTop.z, 0.0, 1.0));
  float bottom = mix(veilBottom.x, veilBottom.y, clamp(uv.y / veilBottom.z, 0.0, 1.0));
  return fieldOpacity * (1.0 - top) * (1.0 - bottom);
}

// The sheet, cell by cell, in the hero's grid.
vec4 sheet(vec2 p) {
  vec2 id = floor(p / cellSize);
  float d = 1e5;
  float nearest = 1e5;
  float lit = 0.0;
  float broken = 0.0;
  float onField = 0.0;
  float fade = 0.0;

  for (int j = -1; j <= 1; j++) {
    for (int i = -1; i <= 1; i++) {
      vec2 cid = id + vec2(float(i), float(j));
      vec2 center = (cid + 0.5) * cellSize;
      float h = hash(cid);

      float start = HOLD + SPREAD * diagonal(center / fieldSize) + JITTER * h;
      if (time > start + LINGER + FADE) continue;
      float b = easeOut((time - start) / BREAK);
      float target = b > 0.0 ? heroField(center, h) : 0.0;
      float amount = mix(1.0, target, b);
      if (amount <= 0.02) continue;

      // Whole cells overlap a little and stay square, so the sheet is solid;
      // below the field's cap a module takes the hero's rounding.
      float halfSize = cellSize * mix(0.10, 0.52, amount);
      float rounding = amount <= CAP
        ? mix(0.92, 0.14, amount)
        : mix(mix(0.92, 0.14, CAP), 0.0, (amount - CAP) / (1.0 - CAP));
      float cd = roundedBox(p - center, halfSize, halfSize * rounding);
      float k = cellSize * 0.16 * (1.0 - b);
      d = k > 0.001 ? smin(d, cd, k) : min(d, cd);

      lit = max(lit, amount);
      if (cd < nearest) {
        nearest = cd;
        broken = b;
        onField = smoothstep(0.0, 0.05, target);
        fade = smoothstep(0.0, 1.0, (time - start - LINGER) / FADE);
      }
    }
  }

  // Unbroken, a cell is the ground. Broken, it lights as the hero's modules do,
  // dimmer if it is leaving. It is drawn opaque, so a module that stays covers
  // the identical hero module beneath and its fade cannot be seen.
  float coverage = smoothstep(0.6, -0.6, d);
  vec2 uv = p / fieldSize;
  float strength = mix(0.35, 1.0, clamp(lit / CAP, 0.0, 1.0)) * mix(FLARE, 1.0, onField * broken);
  vec3 rgb = rampAt(diagonal(uv)) * strength * broken * throughVeil(uv);
  return vec4(rgb, 1.0) * coverage * (1.0 - fade);
}

// The mark: solid at first, then its modules come apart and shrink away, the
// hero's resolve in reverse.
vec4 mark(vec2 px, vec2 p) {
  float assemble = 1.0 - easeInOut((time - HOLD + MARK_LEAD) / MARK_TIME);
  if (assemble <= 0.0) return vec4(0.0);
  vec2 mu = toMark(px);
  if (mu.x < -1.0 || mu.x > 5.0 || mu.y < -1.0 || mu.y > 6.0) return vec4(0.0);

  vec2 id = floor(p / cellSize);
  float d = 1e5;
  float lit = 0.0;
  float k = cellSize * 0.16 * assemble;
  float trim = smoothstep(0.55, 1.0, assemble);

  for (int j = -1; j <= 1; j++) {
    for (int i = -1; i <= 1; i++) {
      vec2 cid = id + vec2(float(i), float(j));
      vec2 center = (cid + 0.5) * cellSize;
      float md0 = markDist(toMark(center + fieldOrigin));
      float inside = 1.0 - smoothstep(-0.015, 0.015, md0);
      float ring = 1.0 - smoothstep(-0.015, 0.015, md0 - 0.75 * cellSize / markUnit);
      float amount = mix(inside, ring, trim) * assemble;
      if (amount <= 0.02) continue;

      float halfSize = cellSize * mix(0.10, 0.52, amount);
      float radius = halfSize * mix(0.92, 0.14, amount);
      float cd = roundedBox(p - center, halfSize, radius);
      d = k > 0.001 ? smin(d, cd, k) : min(d, cd);
      lit = max(lit, amount);
    }
  }

  if (trim > 0.001) {
    float md = markDist(mu) * markUnit;
    d = mix(d, max(d, md), trim);
  }

  // The mark carries the ramp across itself, as the drawn mark does; its loose
  // modules take the field's.
  float gMark = clamp((4.0 * mu.x + 5.0 * mu.y) / 41.0, 0.0, 1.0);
  float strength = mix(mix(0.35, 1.0, clamp(lit / CAP, 0.0, 1.0)), 1.0, assemble);
  float alpha = smoothstep(0.6, -0.6, d) * strength;
  return vec4(rampAt(mix(diagonal(p / fieldSize), gMark, assemble)) * alpha, alpha);
}

void main() {
  vec2 px = gl_FragCoord.xy;
  vec2 p = px - fieldOrigin;
  vec4 top = mark(px, p);
  vec4 under = sheet(p);
  gl_FragColor = top + under * (1.0 - top.a);
}
`;

// Seconds until the last module has cleared: HOLD + SPREAD + JITTER + LINGER +
// FADE (1.85), rounded up.
const END = 1.9;
// The most one frame may move the opening on, in ms (about two frames), so a
// late frame never skips the hold.
const MAX_STEP = 34;

// The hero's veil and canvas opacity, as .hero-veil and .hero-scene set them in
// globals.css, at each width: alpha at the edge, alpha past the stop, the stop
// as a share of the height.
const HERO_VEIL = {
  wide: { top: [0.86, 0, 0.28], bottom: [0.92, 0, 0.44], opacity: 1 },
  narrow: { top: [0.88, 0.3, 0.34], bottom: [0.92, 0.35, 0.46], opacity: 0.85 },
} as const;
const NO_VEIL = { top: [0, 0, 1], bottom: [0, 0, 1], opacity: 1 } as const;

/** The opening in any positioned box; a new key plays it again. */
export function FieldCurtain({
  className = "",
  alignTo,
  clearLeft = false,
  heroVeil = false,
  play = true,
  dismiss = false,
  onDone,
}: {
  className?: string;
  /** Selector for the ModuleField canvas to land on; without one the sheet clears to nothing. */
  alignTo?: string;
  /** Keep the left clear for a headline, as the hero does. */
  clearLeft?: boolean;
  /** Match the home page hero's veil and opacity. */
  heroVeil?: boolean;
  /** Hold the cover until this is true. */
  play?: boolean;
  /** Fade the cover out without playing. */
  dismiss?: boolean;
  onDone?: () => void;
}) {
  const hostRef = useRef<HTMLDivElement>(null);
  const canvasRef = useRef<HTMLCanvasElement>(null);
  const markRef = useRef<HTMLSpanElement>(null);
  const onDoneRef = useRef(onDone);
  onDoneRef.current = onDone;
  const [mode, setMode] = useState<"cover" | "live" | "fading" | "done">("cover");

  useEffect(() => {
    let fadeTimer = 0;
    const fadeOut = () => {
      setMode("fading");
      fadeTimer = window.setTimeout(() => {
        setMode("done");
        onDoneRef.current?.();
      }, 600);
    };
    if (dismiss) {
      fadeOut();
      return () => window.clearTimeout(fadeTimer);
    }

    if (!play) return;
    const host = hostRef.current;
    const canvas = canvasRef.current;
    const markEl = markRef.current;
    if (!host || !canvas || !markEl) return;
    // JS has the cover now: turn off the CSS release meant for a page whose JS
    // never runs.
    host.dataset.armed = "";

    const gl = canvas.getContext("webgl", { alpha: true, antialias: false });
    const field = gl ? createFieldProgram(gl, FRAG, "FieldCurtain") : null;
    if (!gl || !field) {
      fadeOut();
      return () => window.clearTimeout(fadeTimer);
    }
    const { program } = field;
    const uTime = gl.getUniformLocation(program, "time");
    const uFieldTime = gl.getUniformLocation(program, "fieldTime");
    const uCell = gl.getUniformLocation(program, "cellSize");
    const uMarkOrigin = gl.getUniformLocation(program, "markOrigin");
    const uMarkUnit = gl.getUniformLocation(program, "markUnit");
    const uFieldOrigin = gl.getUniformLocation(program, "fieldOrigin");
    const uFieldSize = gl.getUniformLocation(program, "fieldSize");
    const uVeilTop = gl.getUniformLocation(program, "veilTop");
    const uVeilBottom = gl.getUniformLocation(program, "veilBottom");
    const uFieldOpacity = gl.getUniformLocation(program, "fieldOpacity");
    gl.uniform1f(gl.getUniformLocation(program, "clearing"), clearLeft ? 1 : 0);

    // When the target field's clock started; set by measure().
    let fieldClock = Number.NaN;

    // Sizes come from the page itself: the canvas from its box, the mark from
    // the cover's mark, the grid from the field it lands on.
    const measure = () => {
      const rect = host.getBoundingClientRect();
      const dpr = Math.min(window.devicePixelRatio || 1, 2);
      const w = Math.max(1, Math.floor(rect.width * dpr));
      const h = Math.max(1, Math.floor(rect.height * dpr));
      canvas.width = w;
      canvas.height = h;
      gl.viewport(0, 0, w, h);
      gl.uniform1f(uCell, fieldCellSize(dpr));

      const m = markEl.getBoundingClientRect();
      gl.uniform1f(uMarkUnit, (m.width * dpr) / 4);
      gl.uniform2f(uMarkOrigin, (m.left - rect.left) * dpr, (rect.bottom - m.bottom) * dpr);

      const target = alignTo ? document.querySelector<HTMLCanvasElement>(alignTo) : null;
      fieldClock = Number(target?.dataset.clock);
      const box = target?.getBoundingClientRect();
      const fits = !!box && box.width > 0 && box.height > 0;
      if (box && fits) {
        // ModuleField sizes its canvas the same way, so this is its grid exactly.
        gl.uniform2f(uFieldOrigin, (box.left - rect.left) * dpr, (rect.bottom - box.bottom) * dpr);
        gl.uniform2f(uFieldSize, Math.max(1, Math.floor(box.width * dpr)), Math.max(1, Math.floor(box.height * dpr)));
      } else {
        gl.uniform2f(uFieldOrigin, 0, 0);
        gl.uniform2f(uFieldSize, w, h);
      }
      const narrow = window.matchMedia("(max-width: 900px)").matches;
      const veil = fits && heroVeil ? (narrow ? HERO_VEIL.narrow : HERO_VEIL.wide) : NO_VEIL;
      gl.uniform3f(uVeilTop, veil.top[0], veil.top[1], veil.top[2]);
      gl.uniform3f(uVeilBottom, veil.bottom[0], veil.bottom[1], veil.bottom[2]);
      gl.uniform1f(uFieldOpacity, veil.opacity);
    };

    // The page keeps several field canvases running, so the context goes back
    // as soon as the opening is over. Not on cleanup: in development React runs
    // the effect twice, and the second run would get the lost context back.
    let released = false;
    const release = () => {
      if (released) return;
      released = true;
      field.dispose();
      gl.getExtension("WEBGL_lose_context")?.loseContext();
    };

    let frame = 0;
    let started = 0;
    let last = 0;
    let t = 0;
    const draw = (now: number) => {
      // The clock starts on the first frame, so a tab opened in the background
      // plays the opening when it is first seen.
      if (!started) {
        started = last = now;
        measure();
      }
      t += Math.min(now - last, MAX_STEP) / 1000;
      last = now;
      gl.uniform1f(uTime, t);
      gl.uniform1f(uFieldTime, Number.isFinite(fieldClock) ? (now - fieldClock) / 1000 : t);
      gl.drawArrays(gl.TRIANGLES, 0, 6);
      if (t === 0) setMode("live");
      if (t >= END) {
        release();
        setMode("done");
        onDoneRef.current?.();
        return;
      }
      frame = requestAnimationFrame(draw);
    };
    frame = requestAnimationFrame(draw);

    const resizeObserver = new ResizeObserver(() => {
      if (started) measure();
    });
    resizeObserver.observe(host);

    return () => {
      cancelAnimationFrame(frame);
      resizeObserver.disconnect();
      if (!released) field.dispose();
    };
  }, [alignTo, clearLeft, heroVeil, play, dismiss]);

  if (mode === "done") return null;

  return (
    <div ref={hostRef} className={`curtain ${className}`} data-mode={mode} aria-hidden>
      <div className="curtain-cover" />
      <div className="curtain-brand">
        <span ref={markRef} className="curtain-mark">
          <CovenantMark />
        </span>
      </div>
      <canvas ref={canvasRef} className="curtain-canvas" />
    </div>
  );
}

// The opening plays on a full load of the home page; coming back to it from
// another page on the site lands on it directly.
let opened = false;

/** The opening on the home page: over the whole window, landing on the hero's field. */
export function ModuleCurtain() {
  const [state, setState] = useState<"pending" | "playing" | "fading" | "done">(() =>
    opened ? "done" : "pending",
  );

  // Decided before the canvas starts. Reduced motion lands on the page at once
  // (its cover is never shown). A page that took a while to start, or that the
  // browser restored part-way down with no hero in view to land on, fades the
  // cover out instead; after 4 s the cover is already letting the page through
  // on its own.
  useLayoutEffect(() => {
    if (state !== "pending") return;
    opened = true;
    const reduced = window.matchMedia("(prefers-reduced-motion: reduce)").matches;
    const age = performance.now();
    if (reduced || age > 4000) setState("done");
    else setState(age > 3500 || window.scrollY > 0 ? "fading" : "playing");
  }, [state]);

  // Scrolling waits for the opening; a scroll that still gets through (a late
  // scroll restore) fades the cover out.
  useEffect(() => {
    if (state !== "playing") return;
    const html = document.documentElement;
    const onScroll = () => {
      if (window.scrollY > 0) setState("fading");
    };
    html.classList.add("is-loading");
    window.addEventListener("scroll", onScroll, { passive: true });
    return () => {
      html.classList.remove("is-loading");
      window.removeEventListener("scroll", onScroll);
    };
  }, [state]);

  if (state === "done") return null;

  return (
    <>
      <noscript>
        <style>{".curtain-window{display:none}"}</style>
      </noscript>
      <FieldCurtain
        className="curtain-window"
        alignTo=".hero-scene canvas"
        clearLeft
        heroVeil
        play={state === "playing"}
        dismiss={state === "fading"}
        onDone={() => setState("done")}
      />
    </>
  );
}
