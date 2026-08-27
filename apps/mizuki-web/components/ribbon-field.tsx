'use client';

import { useEffect, useRef } from 'react';

// The field is drawn as a grid of rounded squares — the logomark's own module.
// Each cell's size tracks the ribbon intensity under it and its corner radius
// melts toward zero as it grows, and neighbours are joined with a smooth union,
// so dense areas fuse into continuous bars the way the mark's modules do.
// Scrolling the stage swaps the driving field from the drifting ribbons to the
// mark's silhouette, so the squares resolve into the logo.
//
// Raw WebGL over a fullscreen triangle pair; pauses off-screen and never starts
// under reduced motion.

const VERT = `
attribute vec2 position;
void main() {
  gl_Position = vec4(position, 0.0, 1.0);
}
`;

const FRAG = `
precision highp float;
uniform vec2 resolution;
uniform float time;
uniform vec2 pointer;
uniform float cellSize;
uniform vec2 markOrigin;
uniform float markUnit;
uniform float assemble;

float ribbon(float x, float y, float offset, float width, float phase) {
  float c = 0.55 + 0.20 * sin((x * 2.15) + phase) + 0.045 * sin((x * 7.0) - phase * 0.7);
  float d = abs(y - c - offset);
  return exp(-(d * d) / width);
}

float ribbons(vec2 uv, float drift) {
  float t = time * 0.22;
  float r1 = ribbon(uv.x + drift, uv.y, 0.03, 0.0075, t + 0.9);
  float r2 = ribbon(uv.x - drift * 0.7, uv.y, -0.23, 0.0095, t + 3.25);
  float r3 = ribbon(uv.x + drift * 0.4, uv.y, 0.25, 0.016, t + 1.85);
  return clamp(r1 * 1.15 + r2 * 1.05 + r3 * 0.6, 0.0, 1.4);
}

float rbox(vec2 p, vec2 c, vec2 b, float r) {
  vec2 q = abs(p - c) - b + r;
  return min(max(q.x, q.y), 0.0) + length(max(q, 0.0)) - r;
}

float sminK(float a, float b, float k) {
  float h = clamp(0.5 + 0.5 * (b - a) / k, 0.0, 1.0);
  return mix(b, a, h) - k * h * (1.0 - h);
}

// The mark, in 210-unit module space: two arches over three stems. The stems
// meet the bars at single points, so the union is smoothed — that reproduces
// the concave fillets the real mark carries at those junctions.
float markDist(vec2 mu) {
  float r = 38.0 / 210.0;
  float k = 0.34;
  float d = rbox(mu, vec2(1.5, 0.5), vec2(1.5, 0.5), r);
  d = sminK(d, rbox(mu, vec2(0.5, 3.0), vec2(0.5, 2.0), r), k);
  d = sminK(d, rbox(mu, vec2(3.5, 3.0), vec2(0.5, 2.0), r), k);
  d = sminK(d, rbox(mu, vec2(5.0, 0.5), vec2(1.0, 0.5), r), k);
  d = sminK(d, rbox(mu, vec2(6.5, 3.0), vec2(0.5, 2.0), r), k);
  return d;
}

// gl_FragCoord is y-up; the mark is defined y-down, so the axis is flipped.
vec2 toMark(vec2 px) {
  vec2 m = (px - markOrigin) / markUnit;
  return vec2(m.x, 5.0 - m.y);
}

float markMask(vec2 mu) {
  return 1.0 - smoothstep(-0.015, 0.015, markDist(mu));
}

float intensity(vec2 px, float drift) {
  vec2 uv = px / resolution;
  float r = ribbons(uv, drift) * smoothstep(0.22, 0.68, uv.x);
  float m = markMask(toMark(px));
  float halftone = m * clamp(0.30 + r * 0.95, 0.22, 1.0);
  float field = mix(r, halftone, assemble);

  // The cursor carries the field with it: modules under the pointer swell and
  // fuse, so moving the mouse writes bars across the page.
  float pd = length(px - pointer * resolution) / (markUnit * 1.35);
  float torch = exp(-pd * pd) * 1.05;
  return field + torch * (1.0 - assemble * 0.45);
}

float roundedBox(vec2 p, float halfSize, float r) {
  vec2 q = abs(p) - halfSize + r;
  return min(max(q.x, q.y), 0.0) + length(max(q, 0.0)) - r;
}

float smin(float a, float b, float k) {
  float h = clamp(0.5 + 0.5 * (b - a) / k, 0.0, 1.0);
  return mix(b, a, h) - k * h * (1.0 - h);
}

void main() {
  vec2 px = gl_FragCoord.xy;
  float drift = (pointer.x - 0.5) * 0.06;

  vec2 id = floor(px / cellSize);
  float d = 1e5;
  float lit = 0.0;

  for (int j = -1; j <= 1; j++) {
    for (int i = -1; i <= 1; i++) {
      vec2 cid = id + vec2(float(i), float(j));
      vec2 center = (cid + 0.5) * cellSize;
      float e = intensity(center, drift);

      float wob = 0.5 + 0.5 * sin(time * 0.6 + cid.x * 0.7 + cid.y * 1.1);
      float amount = clamp(e * (0.82 + 0.18 * wob), 0.0, 1.0);
      if (amount <= 0.001) continue;

      float halfSize = cellSize * mix(0.10, 0.52, amount);
      float radius = halfSize * mix(0.92, 0.14, amount);
      float cd = roundedBox(px - center, halfSize, radius);

      d = smin(d, cd, cellSize * 0.16);
      lit = max(lit, amount);
    }
  }

  // Clip the field to the silhouette as it assembles, then resolve fully onto
  // the mark's own distance field so the settled frame is a clean edge rather
  // than the cell grid's serration.
  if (assemble > 0.001) {
    float md = markDist(toMark(px)) * markUnit;
    d = mix(d, max(d, md), assemble);
  }

  float alpha = smoothstep(0.6, -0.6, d);

  float g = clamp((px.x / resolution.x) * 0.62 + (1.0 - px.y / resolution.y) * 0.38, 0.0, 1.0);
  vec3 violet = vec3(0.627, 0.231, 0.941);
  vec3 blue = vec3(0.298, 0.498, 0.871);
  vec3 mint = vec3(0.114, 0.890, 0.682);
  vec3 ramp = g < 0.5 ? mix(violet, blue, g * 2.0) : mix(blue, mint, (g - 0.5) * 2.0);

  // Light page: the field paints onto paper rather than out of the dark.
  vec3 base = vec3(0.957, 0.949, 0.976);
  vec3 col = mix(base, ramp, alpha * clamp(0.45 + 0.75 * lit, 0.0, 1.0));

  gl_FragColor = vec4(col, 1.0);
}
`;

function compile(gl: WebGLRenderingContext, type: number, src: string) {
  const shader = gl.createShader(type);
  if (!shader) throw new Error('Unable to create shader');
  gl.shaderSource(shader, src);
  gl.compileShader(shader);
  if (!gl.getShaderParameter(shader, gl.COMPILE_STATUS)) {
    throw new Error(gl.getShaderInfoLog(shader) ?? 'Shader compilation failed');
  }
  return shader;
}

const MARK_W = 7;
const MARK_H = 5;

export function RibbonField({ className = '' }: { className?: string }) {
  const hostRef = useRef<HTMLDivElement>(null);
  const canvasRef = useRef<HTMLCanvasElement>(null);

  useEffect(() => {
    const host = hostRef.current;
    const canvas = canvasRef.current;
    if (!host || !canvas) return;
    const reduced = window.matchMedia('(prefers-reduced-motion: reduce)').matches;

    const gl = canvas.getContext('webgl', { alpha: true, antialias: false });
    if (!gl) return;

    let vs: WebGLShader;
    let fs: WebGLShader;
    try {
      vs = compile(gl, gl.VERTEX_SHADER, VERT);
      fs = compile(gl, gl.FRAGMENT_SHADER, FRAG);
    } catch (error) {
      console.error('[RibbonField]', error);
      return;
    }

    const program = gl.createProgram();
    if (!program) return;
    gl.attachShader(program, vs);
    gl.attachShader(program, fs);
    gl.linkProgram(program);
    if (!gl.getProgramParameter(program, gl.LINK_STATUS)) return;
    gl.useProgram(program);

    const buffer = gl.createBuffer();
    gl.bindBuffer(gl.ARRAY_BUFFER, buffer);
    gl.bufferData(
      gl.ARRAY_BUFFER,
      new Float32Array([-1, -1, 1, -1, -1, 1, -1, 1, 1, -1, 1, 1]),
      gl.STATIC_DRAW,
    );
    const posLoc = gl.getAttribLocation(program, 'position');
    gl.enableVertexAttribArray(posLoc);
    gl.vertexAttribPointer(posLoc, 2, gl.FLOAT, false, 0, 0);

    const uResolution = gl.getUniformLocation(program, 'resolution');
    const uTime = gl.getUniformLocation(program, 'time');
    const uPointer = gl.getUniformLocation(program, 'pointer');
    const uCell = gl.getUniformLocation(program, 'cellSize');
    const uMarkOrigin = gl.getUniformLocation(program, 'markOrigin');
    const uMarkUnit = gl.getUniformLocation(program, 'markUnit');
    const uAssemble = gl.getUniformLocation(program, 'assemble');

    let px = 0.72;
    let py = 0.42;
    let tx = 0.72;
    let ty = 0.42;
    let frame = 0;
    let visible = true;
    const started = performance.now();

    const onPointer = (event: PointerEvent) => {
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
      gl.uniform1f(uCell, Math.max(12, Math.round(15 * dpr)));

      const unit = Math.min(w / (MARK_W + 3.2), h / (MARK_H + 2.2));
      gl.uniform1f(uMarkUnit, unit);
      gl.uniform2f(uMarkOrigin, (w - MARK_W * unit) / 2, (h - MARK_H * unit) / 2);
    };

    const assembleProgress = () => {
      if (reduced) return 1;
      const stage = host.closest('.hero-stage') as HTMLElement | null;
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

    resizeObserver.observe(host);
    intersectionObserver.observe(host);
    host.addEventListener('pointermove', onPointer, { passive: true });
    resize();
    frame = requestAnimationFrame(draw);

    return () => {
      if (frame) cancelAnimationFrame(frame);
      resizeObserver.disconnect();
      intersectionObserver.disconnect();
      host.removeEventListener('pointermove', onPointer);
      gl.deleteBuffer(buffer);
      gl.deleteShader(vs);
      gl.deleteShader(fs);
      gl.deleteProgram(program);
    };
  }, []);

  return (
    <div ref={hostRef} className={className} aria-hidden>
      <canvas ref={canvasRef} />
    </div>
  );
}
