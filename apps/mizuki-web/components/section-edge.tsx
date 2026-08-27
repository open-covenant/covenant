'use client';

import { useEffect, useRef } from 'react';

// The boundary between a paper section and a dark one, drawn with the same
// module field as the hero instead of a straight rule: black modules thin out
// across the strip so the band dissolves into the page.

const VERT = `
attribute vec2 position;
void main() {
  gl_Position = vec4(position, 0.0, 1.0);
}
`;

const FRAG = `
precision highp float;
uniform vec2 resolution;
uniform float cellSize;
uniform float flip;
uniform float seed;

float hash(vec2 p) {
  p = fract(p * vec2(123.34, 456.21));
  p += dot(p, p + 45.32);
  return fract(p.x * p.y);
}

float roundedBox(vec2 p, float halfSize, float r) {
  vec2 q = abs(p) - halfSize + r;
  return min(max(q.x, q.y), 0.0) + length(max(q, 0.0)) - r;
}

float smin(float a, float b, float k) {
  float h = clamp(0.5 + 0.5 * (b - a) / k, 0.0, 1.0);
  return mix(b, a, h) - k * h * (1.0 - h);
}

// Density across the strip: solid on the section side, gone on the page side.
float density(vec2 px) {
  vec2 uv = px / resolution;
  float t = flip > 0.5 ? 1.0 - uv.y : uv.y;
  // Reaches full coverage well before the section edge, so the band meets the
  // solid ground seamlessly and only the dissolve is visible.
  float edge = smoothstep(0.0, 0.78, t);
  // A slow horizontal wave so the boundary is not a flat gradient.
  float wave = 0.5 + 0.5 * sin(uv.x * 6.2 + seed);
  float wave2 = 0.5 + 0.5 * sin(uv.x * 2.3 - seed * 1.7);
  return clamp(edge * (1.18 + 0.42 * wave * wave2) - 0.05, 0.0, 1.0);
}

void main() {
  vec2 px = gl_FragCoord.xy;
  vec2 id = floor(px / cellSize);
  float d = 1e5;

  for (int j = -1; j <= 1; j++) {
    for (int i = -1; i <= 1; i++) {
      vec2 cid = id + vec2(float(i), float(j));
      vec2 center = (cid + 0.5) * cellSize;
      float amount = density(center);
      amount *= 0.84 + 0.16 * hash(cid);
      if (amount <= 0.002) continue;

      float halfSize = cellSize * mix(0.06, 0.62, amount);
      float radius = halfSize * mix(0.92, 0.16, amount);
      d = smin(d, roundedBox(px - center, halfSize, radius), cellSize * 0.16);
    }
  }

  float alpha = smoothstep(0.6, -0.6, d);
  gl_FragColor = vec4(0.051, 0.043, 0.086, alpha);
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

export function SectionEdge({
  position = 'top',
  seed = 0,
}: {
  position?: 'top' | 'bottom';
  seed?: number;
}) {
  const hostRef = useRef<HTMLDivElement>(null);
  const canvasRef = useRef<HTMLCanvasElement>(null);

  useEffect(() => {
    const host = hostRef.current;
    const canvas = canvasRef.current;
    if (!host || !canvas) return;

    // This edge draws once rather than per frame, so the drawing buffer has to
    // survive compositing — otherwise the single draw is discarded and the
    // canvas composites empty.
    const gl = canvas.getContext('webgl', {
      alpha: true,
      antialias: false,
      premultipliedAlpha: false,
      preserveDrawingBuffer: true,
    });
    if (!gl) return;

    let vs: WebGLShader;
    let fs: WebGLShader;
    try {
      vs = compile(gl, gl.VERTEX_SHADER, VERT);
      fs = compile(gl, gl.FRAGMENT_SHADER, FRAG);
    } catch (error) {
      console.error('[SectionEdge]', error);
      return;
    }

    const program = gl.createProgram();
    if (!program) return;
    gl.attachShader(program, vs);
    gl.attachShader(program, fs);
    gl.linkProgram(program);
    if (!gl.getProgramParameter(program, gl.LINK_STATUS)) return;
    gl.useProgram(program);
    gl.enable(gl.BLEND);
    gl.blendFunc(gl.SRC_ALPHA, gl.ONE_MINUS_SRC_ALPHA);

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

    gl.uniform1f(gl.getUniformLocation(program, 'flip'), position === 'top' ? 1 : 0);
    gl.uniform1f(gl.getUniformLocation(program, 'seed'), seed);
    const uResolution = gl.getUniformLocation(program, 'resolution');
    const uCell = gl.getUniformLocation(program, 'cellSize');

    // Static: the edge does not animate, so it draws once per size change.
    const render = () => {
      const rect = host.getBoundingClientRect();
      if (rect.width < 1 || rect.height < 1) return;
      const dpr = Math.min(window.devicePixelRatio || 1, 2);
      const w = Math.max(1, Math.floor(rect.width * dpr));
      const h = Math.max(1, Math.floor(rect.height * dpr));
      canvas.width = w;
      canvas.height = h;
      gl.viewport(0, 0, w, h);
      gl.uniform2f(uResolution, w, h);
      gl.uniform1f(uCell, Math.max(12, Math.round(15 * dpr)));
      gl.clearColor(0, 0, 0, 0);
      gl.clear(gl.COLOR_BUFFER_BIT);
      gl.drawArrays(gl.TRIANGLES, 0, 6);
    };

    const observer = new ResizeObserver(render);
    observer.observe(host);
    render();

    return () => {
      observer.disconnect();
      gl.deleteBuffer(buffer);
      gl.deleteShader(vs);
      gl.deleteShader(fs);
      gl.deleteProgram(program);
    };
  }, [position, seed]);

  return (
    <div ref={hostRef} className={`section-edge is-${position}`} aria-hidden>
      <canvas ref={canvasRef} />
    </div>
  );
}
