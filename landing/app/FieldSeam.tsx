"use client";

import { useEffect, useRef } from "react";
import { createFieldProgram, FIELD_GLSL, fieldCellSize } from "./_field";

// The seam between two sections: a sparse strip of the hero's field. The
// ribbons drift and flow with the scroll; each cell lights only when enough
// ribbon passes under it, and then breathes on its own clock. Modules are
// capped well short of the cell, so they never fuse: the strip stays a scatter
// that thins out at its top and bottom and never closes into a solid band.
// Scrolling past it flares it up, and the pointer swells the modules under it
// from a little way above or below.
//
// Animates only while on screen; under reduced motion it draws one still frame.

const FRAG = `
precision highp float;
uniform vec2 resolution;
uniform float time;
uniform float flow;
uniform float energy;
uniform float cellSize;
uniform float span;
uniform float seed;
uniform vec2 pointer;
uniform float torch;

${FIELD_GLSL}

float hash(vec2 p) {
  p = fract(p * vec2(123.34, 456.21));
  p += dot(p, p + 45.32);
  return fract(p.x * p.y);
}

// How much the field wants a module here: the ribbons, a swell that travels
// along the strip, the scroll's energy and the pointer, thinned toward the edges.
float intensity(vec2 px) {
  vec2 uv = vec2(px.x / resolution.x, 0.55 + (px.y - resolution.y * 0.5) / span);
  float x = uv.x;
  float r = ribbons(vec2(x + flow * 0.04, uv.y), 0.0, time * 0.34 + flow + seed) * 0.7;

  float lead = fract(time * 0.05 + seed * 0.37) * 1.4 - 0.2;
  r += exp(-(x - lead) * (x - lead) * 40.0) * 0.4;
  r += energy * 0.4;

  float pd = length(px - pointer) / (cellSize * 7.0);
  r += exp(-pd * pd) * torch;

  float v = px.y / resolution.y;
  return r * smoothstep(0.0, 0.5, v) * smoothstep(0.0, 0.5, 1.0 - v);
}

// The largest a seam module may grow, as a share of the field's range. At this
// cap a module covers about 60% of its cell, so neighbours never touch.
const float CAP = 0.5;

void main() {
  vec2 px = gl_FragCoord.xy;
  vec2 cid = floor(px / cellSize);
  vec2 center = (cid + 0.5) * cellSize;
  float e = intensity(center);

  // A cell needs more ribbon under it the higher its hash, so even the
  // densest part of the strip is a scatter, and every module breathes on a
  // clock of its own.
  float h = hash(cid + seed * 17.0);
  float gate = smoothstep(0.1 + h * 0.9, 0.3 + h * 0.9, e);
  float breathe = 0.5 + 0.5 * sin(time * (0.8 + h * 1.6) + h * 6.2831);
  float amount = min(e * gate * mix(0.4, 1.0, breathe), CAP);

  float alpha = 0.0;
  if (amount > 0.02) {
    float halfSize = cellSize * mix(0.10, 0.52, amount);
    float radius = halfSize * mix(0.92, 0.14, amount);
    float d = roundedBox(px - center, halfSize, radius);
    alpha = smoothstep(0.6, -0.6, d) * mix(0.35, 1.0, amount / CAP);
  }

  // The outermost rows fade as well as shrink, so the strip has no edge of its
  // own: the dots thin into the ground rather than stopping on a line.
  float v = px.y / resolution.y;
  alpha *= smoothstep(0.0, 0.3, v) * smoothstep(0.0, 0.3, 1.0 - v);
  gl_FragColor = vec4(rampAt(px.x / resolution.x) * alpha, alpha);
}
`;

export function FieldSeam({ seed = 0, className = "" }: { seed?: number; className?: string }) {
  const hostRef = useRef<HTMLDivElement>(null);
  const canvasRef = useRef<HTMLCanvasElement>(null);

  useEffect(() => {
    const host = hostRef.current;
    const canvas = canvasRef.current;
    if (!host || !canvas) return;
    const reduced = window.matchMedia("(prefers-reduced-motion: reduce)").matches;

    // A still frame is drawn once, so its buffer has to survive compositing or
    // the canvas composites empty.
    const gl = canvas.getContext("webgl", { alpha: true, antialias: false, preserveDrawingBuffer: reduced });
    if (!gl) return;

    const field = createFieldProgram(gl, FRAG, "FieldSeam");
    if (!field) return;
    const { program } = field;

    gl.uniform1f(gl.getUniformLocation(program, "seed"), seed);
    const uResolution = gl.getUniformLocation(program, "resolution");
    const uTime = gl.getUniformLocation(program, "time");
    const uFlow = gl.getUniformLocation(program, "flow");
    const uEnergy = gl.getUniformLocation(program, "energy");
    const uCell = gl.getUniformLocation(program, "cellSize");
    const uSpan = gl.getUniformLocation(program, "span");
    const uPointer = gl.getUniformLocation(program, "pointer");
    const uTorch = gl.getUniformLocation(program, "torch");

    let dpr = 1;
    let pointerX = 0;
    let pointerY = 0;
    let torch = 0;
    let torchTarget = 0;
    let energy = 0;
    let energyTarget = 0;
    let clock = seed * 9;
    let last = performance.now();
    let lastY = window.scrollY;
    let lastScroll = last;
    let frame = 0;
    let visible = false;

    const render = () => {
      gl.uniform1f(uTime, clock);
      gl.uniform1f(uFlow, window.scrollY * 0.0016);
      gl.uniform1f(uEnergy, energy);
      gl.uniform2f(uPointer, pointerX, pointerY);
      gl.uniform1f(uTorch, torch);
      gl.drawArrays(gl.TRIANGLES, 0, 6);
    };

    const resize = () => {
      const rect = host.getBoundingClientRect();
      if (rect.width < 1 || rect.height < 1) return;
      dpr = Math.min(window.devicePixelRatio || 1, 2);
      const w = Math.max(1, Math.floor(rect.width * dpr));
      const h = Math.max(1, Math.floor(rect.height * dpr));
      canvas.width = w;
      canvas.height = h;
      gl.viewport(0, 0, w, h);
      gl.uniform2f(uResolution, w, h);
      gl.uniform1f(uCell, fieldCellSize(dpr));
      // The hero's field is a viewport tall; the strip is a slice of one.
      gl.uniform1f(uSpan, Math.max(window.innerHeight, 480) * dpr);
      if (reduced) render();
    };

    const tick = (now: number) => {
      const dt = Math.min((now - last) / 1000, 0.1);
      last = now;
      // Scroll energy flares fast and settles slowly; while it is up the
      // modules breathe faster too.
      energyTarget *= 0.9;
      energy += (energyTarget - energy) * (energyTarget > energy ? 0.25 : 0.05);
      torch += (torchTarget - torch) * 0.08;
      clock += dt * (1 + energy * 2.5);
      render();
      frame = visible && !document.hidden ? requestAnimationFrame(tick) : 0;
    };

    const start = () => {
      if (!reduced && visible && !document.hidden && !frame) {
        last = performance.now();
        frame = requestAnimationFrame(tick);
      }
    };

    const stop = () => {
      if (frame) cancelAnimationFrame(frame);
      frame = 0;
    };

    const onScroll = () => {
      const now = performance.now();
      const y = window.scrollY;
      const speed = Math.abs(y - lastY) / Math.max(now - lastScroll, 8);
      lastY = y;
      lastScroll = now;
      energyTarget = Math.max(energyTarget, Math.min(speed * 0.5, 1));
    };

    // The pointer reaches the strip from up to 160px above or below it; its
    // height is pinned inside the strip so the modules beneath it respond.
    const onPointer = (event: PointerEvent) => {
      if (!visible) return;
      const rect = host.getBoundingClientRect();
      const outside = Math.max(rect.top - event.clientY, event.clientY - rect.bottom, 0);
      torchTarget = Math.max(0, 1 - outside / 160);
      pointerX = (event.clientX - rect.left) * dpr;
      pointerY = Math.min(Math.max(rect.bottom - event.clientY, 0), rect.height) * dpr;
    };

    const onLeave = () => {
      torchTarget = 0;
    };

    const onVisibility = () => (document.hidden ? stop() : start());

    const resizeObserver = new ResizeObserver(resize);
    const intersectionObserver = new IntersectionObserver(
      ([entry]) => {
        visible = entry?.isIntersecting ?? false;
        if (visible) start();
        else stop();
      },
      { rootMargin: "80px 0px" },
    );

    resizeObserver.observe(host);
    intersectionObserver.observe(host);
    if (!reduced) {
      window.addEventListener("scroll", onScroll, { passive: true });
      window.addEventListener("pointermove", onPointer, { passive: true });
      document.documentElement.addEventListener("pointerleave", onLeave);
    }
    document.addEventListener("visibilitychange", onVisibility);
    resize();

    return () => {
      stop();
      resizeObserver.disconnect();
      intersectionObserver.disconnect();
      window.removeEventListener("scroll", onScroll);
      window.removeEventListener("pointermove", onPointer);
      document.documentElement.removeEventListener("pointerleave", onLeave);
      document.removeEventListener("visibilitychange", onVisibility);
      field.dispose();
    };
  }, [seed]);

  return (
    <div ref={hostRef} className={["field-seam", className].filter(Boolean).join(" ")} aria-hidden>
      <canvas ref={canvasRef} />
    </div>
  );
}
