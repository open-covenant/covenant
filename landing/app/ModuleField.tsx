"use client";

import { type RefObject, useEffect, useRef } from "react";
import { createFieldProgram, FIELD_GLSL, fieldCellSize, MARK_GLSL } from "./_field";

// The hero field, drawn as a grid of rounded squares — the mark's own module.
// As in the seams between sections, a cell lights only when enough ribbon
// passes under it, breathes on its own clock and stays short of its cell, so
// the field is a scatter of separate modules that flares with the scroll and
// swells under the pointer. Scrolling the stage hands the cells from the
// ribbons to the mark's silhouette: modules outside it fade, modules inside it
// grow to full size and fuse, and the field condenses into the solid logo.
// Outside the hero a `progress` ref (0 to 1) drives the same hand-over.
//
// Raw WebGL over a fullscreen triangle pair, drawn in the ramp onto a
// transparent canvas so the night ground behind it shows through; pauses
// off-screen and settles straight onto the mark under reduced motion.

const FRAG = `
precision highp float;
uniform vec2 resolution;
uniform float time;
uniform vec2 pointer;
uniform float cellSize;
uniform vec2 markOrigin;
uniform float markUnit;
uniform float assemble;
uniform float energy;
uniform float torch;
uniform float clearing;

${FIELD_GLSL}

float hash(vec2 p) {
  p = fract(p * vec2(123.34, 456.21));
  p += dot(p, p + 45.32);
  return fract(p.x * p.y);
}

${MARK_GLSL}

// gl_FragCoord is y-up; the mark is defined y-down, so the axis is flipped.
vec2 toMark(vec2 px) {
  vec2 m = (px - markOrigin) / markUnit;
  return vec2(m.x, 5.0 - m.y);
}

// The field under a cell: the drifting ribbons, kept clear of the headline on
// the left; the pointer, while it is over the hero; and the scroll.
float fieldIntensity(vec2 px, float drift) {
  vec2 uv = px / resolution;
  float clear = mix(1.0, smoothstep(0.22, 0.68, uv.x), clearing);
  float r = ribbons(uv, drift, time * 0.3) * 0.8 * clear;
  float pd = length(px - pointer * resolution) / (markUnit * 1.35);
  r += exp(-pd * pd) * 0.9 * torch;
  return r + energy * 0.35 * clear;
}

// The largest a field module may grow, as a share of its range. At this cap a
// module covers about 60% of its cell, so in the field neighbours never touch.
const float CAP = 0.5;

void main() {
  vec2 px = gl_FragCoord.xy;
  float drift = (pointer.x - 0.5) * 0.06;

  vec2 id = floor(px / cellSize);
  float d = 1e5;
  float lit = 0.0;
  // Modules fuse only as the field resolves; in the field they stay apart.
  float k = cellSize * 0.16 * assemble;
  // Near the end of the resolve the fused modules are trimmed to the mark's
  // own outline (below); a ring of cells just outside it grows in over the
  // same span so the trim always has something to cut.
  float trim = smoothstep(0.55, 1.0, assemble);

  for (int j = -1; j <= 1; j++) {
    for (int i = -1; i <= 1; i++) {
      vec2 cid = id + vec2(float(i), float(j));
      vec2 center = (cid + 0.5) * cellSize;

      // The field: a cell needs more ribbon under it the higher its hash, and
      // breathes on a clock of its own.
      float h = hash(cid);
      float e = fieldIntensity(center, drift);
      float gate = smoothstep(0.1 + h * 0.9, 0.3 + h * 0.9, e);
      float breathe = 0.5 + 0.5 * sin(time * (0.8 + h * 1.6) + h * 6.2831);
      float field = min(e * gate * mix(0.4, 1.0, breathe), CAP);

      // The mark: every cell inside its silhouette, and, as the trim comes in,
      // those whose centre sits within three quarters of a cell outside it, at
      // full size. The trim cuts them back to the mark, so the settled edge is
      // solid rather than serrated.
      float md0 = markDist(toMark(center));
      float inside = 1.0 - smoothstep(-0.015, 0.015, md0);
      float ring = 1.0 - smoothstep(-0.015, 0.015, md0 - 0.75 * cellSize / markUnit);
      float amount = mix(field, mix(inside, ring, trim), assemble);
      if (amount <= 0.02) continue;

      float halfSize = cellSize * mix(0.10, 0.52, amount);
      float radius = halfSize * mix(0.92, 0.14, amount);
      float cd = roundedBox(px - center, halfSize, radius);

      d = k > 0.001 ? smin(d, cd, k) : min(d, cd);
      lit = max(lit, amount);
    }
  }

  // The trim: the fused modules cut back to the mark's own distance field, so
  // the settled frame is a clean edge. Until it starts the modules stay whole.
  if (trim > 0.001) {
    float md = markDist(toMark(px)) * markUnit;
    d = mix(d, max(d, md), trim);
  }

  // Small modules glow faintly and the largest at full strength; the settled
  // mark carries the whole ramp.
  float strength = mix(mix(0.35, 1.0, clamp(lit / CAP, 0.0, 1.0)), 1.0, assemble);
  float alpha = smoothstep(0.6, -0.6, d) * strength;

  float g = clamp((px.x / resolution.x) * 0.62 + (1.0 - px.y / resolution.y) * 0.38, 0.0, 1.0);

  // Premultiplied: the canvas composites over the night ground behind it.
  gl_FragColor = vec4(rampAt(g) * alpha, alpha);
}
`;

// The mark's extent in modules.
const MARK_W = 4;
const MARK_H = 5;

export function ModuleField({
  className = "",
  progress,
  clearLeft = true,
}: {
  className?: string;
  progress?: RefObject<number>;
  /** Keep the left of the frame clear for a headline, as the hero does. */
  clearLeft?: boolean;
}) {
  const hostRef = useRef<HTMLDivElement>(null);
  const canvasRef = useRef<HTMLCanvasElement>(null);

  useEffect(() => {
    const host = hostRef.current;
    const canvas = canvasRef.current;
    if (!host || !canvas) return;
    const reduced = window.matchMedia("(prefers-reduced-motion: reduce)").matches;

    const gl = canvas.getContext("webgl", { alpha: true, antialias: false });
    if (!gl) return;

    const field = createFieldProgram(gl, FRAG, "ModuleField");
    if (!field) return;
    const { program } = field;

    const uResolution = gl.getUniformLocation(program, "resolution");
    const uTime = gl.getUniformLocation(program, "time");
    const uPointer = gl.getUniformLocation(program, "pointer");
    const uCell = gl.getUniformLocation(program, "cellSize");
    const uMarkOrigin = gl.getUniformLocation(program, "markOrigin");
    const uMarkUnit = gl.getUniformLocation(program, "markUnit");
    const uAssemble = gl.getUniformLocation(program, "assemble");
    const uEnergy = gl.getUniformLocation(program, "energy");
    const uTorch = gl.getUniformLocation(program, "torch");
    gl.uniform1f(gl.getUniformLocation(program, "clearing"), clearLeft ? 1 : 0);

    let px = 0.72;
    let py = 0.42;
    let tx = 0.72;
    let ty = 0.42;
    let frame = 0;
    let visible = true;
    const started = performance.now();
    // The opening (ModuleCurtain) reads this to draw the same field.
    canvas.dataset.clock = String(started);
    // The pointer's swell fades in while it is over the hero and out after.
    let torch = 0;
    let torchTarget = 0;
    // Scrolling flares the field, as it does the seams.
    let energy = 0;
    let energyTarget = 0;
    let lastY = window.scrollY;
    let lastScroll = started;

    const onScroll = () => {
      const now = performance.now();
      const speed = Math.abs(window.scrollY - lastY) / Math.max(now - lastScroll, 8);
      lastY = window.scrollY;
      lastScroll = now;
      energyTarget = Math.max(energyTarget, Math.min(speed * 0.5, 1));
    };

    const onLeave = () => {
      torchTarget = 0;
    };

    const onPointer = (event: PointerEvent) => {
      torchTarget = 1;
      const rect = host.getBoundingClientRect();
      tx = (event.clientX - rect.left) / Math.max(rect.width, 1);
      ty = 1 - (event.clientY - rect.top) / Math.max(rect.height, 1);
    };

    const resize = () => {
      const rect = host.getBoundingClientRect();
      // Full device pixels: the settled mark is a hard edge and anything less
      // gets upscaled by the compositor and reads soft.
      const dpr = Math.min(window.devicePixelRatio || 1, 2);
      const w = Math.max(1, Math.floor(rect.width * dpr));
      const h = Math.max(1, Math.floor(rect.height * dpr));
      canvas.width = w;
      canvas.height = h;
      gl.viewport(0, 0, w, h);
      gl.uniform2f(uResolution, w, h);
      gl.uniform1f(uCell, fieldCellSize(dpr));

      const unit = Math.min(w / (MARK_W + 3.2), h / (MARK_H + 2.2));
      gl.uniform1f(uMarkUnit, unit);
      gl.uniform2f(uMarkOrigin, (w - MARK_W * unit) / 2, (h - MARK_H * unit) / 2);
    };

    const assembleProgress = () => {
      if (progress) return Math.min(Math.max(progress.current, 0), 1);
      if (reduced) return 1;
      const stage = host.closest(".hero-stage") as HTMLElement | null;
      if (!stage) return 0;
      const rect = stage.getBoundingClientRect();
      const travel = Math.max(rect.height - window.innerHeight, 1);
      const p = Math.min(Math.max(-rect.top / travel, 0), 1);
      const eased = Math.min(p / 0.95, 1);
      return eased * eased * (3 - 2 * eased);
    };

    const draw = (now: number) => {
      px += (tx - px) * 0.035;
      py += (ty - py) * 0.035;
      gl.uniform1f(uTime, (now - started) * 0.001);
      gl.uniform2f(uPointer, px, py);
      gl.uniform1f(uAssemble, assembleProgress());
      energyTarget *= 0.9;
      energy += (energyTarget - energy) * (energyTarget > energy ? 0.25 : 0.05);
      gl.uniform1f(uEnergy, reduced ? 0 : energy);
      torch += (torchTarget - torch) * 0.06;
      gl.uniform1f(uTorch, torch);
      gl.drawArrays(gl.TRIANGLES, 0, 6);
      frame = visible && !document.hidden ? requestAnimationFrame(draw) : 0;
    };

    const resizeObserver = new ResizeObserver(resize);
    const intersectionObserver = new IntersectionObserver(([entry]) => {
      visible = entry?.isIntersecting ?? true;
      if (visible && !frame) frame = requestAnimationFrame(draw);
      if (!visible && frame) {
        cancelAnimationFrame(frame);
        frame = 0;
      }
    });
    // A hidden tab stops the loop; coming back has to start it again.
    const onVisibility = () => {
      if (!document.hidden && visible && !frame) frame = requestAnimationFrame(draw);
    };

    resizeObserver.observe(host);
    intersectionObserver.observe(host);
    host.addEventListener("pointermove", onPointer, { passive: true });
    host.addEventListener("pointerleave", onLeave);
    window.addEventListener("scroll", onScroll, { passive: true });
    document.addEventListener("visibilitychange", onVisibility);
    resize();
    frame = requestAnimationFrame(draw);

    return () => {
      if (frame) cancelAnimationFrame(frame);
      resizeObserver.disconnect();
      intersectionObserver.disconnect();
      host.removeEventListener("pointermove", onPointer);
      host.removeEventListener("pointerleave", onLeave);
      window.removeEventListener("scroll", onScroll);
      document.removeEventListener("visibilitychange", onVisibility);
      field.dispose();
    };
  }, [progress, clearLeft]);

  return (
    <div ref={hostRef} className={className} aria-hidden>
      <canvas ref={canvasRef} />
    </div>
  );
}
