// The module field, shared by the hero (ModuleField), the seams between
// sections (FieldSeam) and the opening (ModuleCurtain): the WebGL set-up and the
// GLSL they all draw with, so a seam is a strip of the same field rather than a
// lookalike.

export const FIELD_VERT = `
attribute vec2 position;
void main() {
  gl_Position = vec4(position, 0.0, 1.0);
}
`;

// Three drifting ribbons, the rounded module, the smooth union that fuses
// neighbouring modules into bars, and the ramp. `t` is the ribbons' phase.
export const FIELD_GLSL = `
float ribbon(float x, float y, float offset, float width, float phase) {
  float c = 0.55 + 0.20 * sin((x * 2.15) + phase) + 0.045 * sin((x * 7.0) - phase * 0.7);
  float d = abs(y - c - offset);
  return exp(-(d * d) / width);
}

float ribbons(vec2 uv, float drift, float t) {
  float r1 = ribbon(uv.x + drift, uv.y, 0.03, 0.0075, t + 0.9);
  float r2 = ribbon(uv.x - drift * 0.7, uv.y, -0.23, 0.0095, t + 3.25);
  float r3 = ribbon(uv.x + drift * 0.4, uv.y, 0.25, 0.016, t + 1.85);
  return clamp(r1 * 1.15 + r2 * 1.05 + r3 * 0.6, 0.0, 1.4);
}

float roundedBox(vec2 p, float halfSize, float r) {
  vec2 q = abs(p) - halfSize + r;
  return min(max(q.x, q.y), 0.0) + length(max(q, 0.0)) - r;
}

float smin(float a, float b, float k) {
  float h = clamp(0.5 + 0.5 * (b - a) / k, 0.0, 1.0);
  return mix(b, a, h) - k * h * (1.0 - h);
}

vec3 rampAt(float g) {
  vec3 violet = vec3(0.627, 0.231, 0.941);
  vec3 blue = vec3(0.298, 0.498, 0.871);
  vec3 mint = vec3(0.114, 0.890, 0.682);
  return g < 0.5 ? mix(violet, blue, g * 2.0) : mix(blue, mint, (g - 0.5) * 2.0);
}
`;

// The mark's outline as a distance field: the hero condenses into it and the
// opening starts from it.
export const MARK_GLSL = `
// A box with a radius of its own at each corner, in the mark's y-down module
// space: r.x top left, r.y top right, r.z bottom right, r.w bottom left.
float cornerBox(vec2 p, vec2 c, vec2 b, vec4 r) {
  vec2 q = p - c;
  float rad = q.x < 0.0 ? (q.y < 0.0 ? r.x : r.w) : (q.y < 0.0 ? r.y : r.z);
  vec2 d = abs(q) - b + rad;
  return min(max(d.x, d.y), 0.0) + length(max(d, 0.0)) - rad;
}

// A union that rounds the inside corner where two edges meet at a right angle
// with an exact circular fillet of radius r.
float filletUnion(float a, float b, float r) {
  vec2 u = max(vec2(r - a, r - b), vec2(0.0));
  return max(r, min(a, b)) - length(u);
}

// The mark, in 210-unit module space (4 x 5): a top bar, a stem and a bottom
// bar with 38-unit outer corners, square where they meet, then joined by
// 38-unit circular fillets on both sides of each junction, exactly as the
// drawn mark is. The core stays a separate min: it never touches.
float markDist(vec2 mu) {
  float r = 38.0 / 210.0;
  float top = cornerBox(mu, vec2(2.5, 0.5), vec2(1.5, 0.5), vec4(r, r, r, 0.0));
  float stem = cornerBox(mu, vec2(0.5, 2.5), vec2(0.5, 1.5), vec4(r, 0.0, 0.0, r));
  float bottom = cornerBox(mu, vec2(2.5, 4.5), vec2(1.5, 0.5), vec4(0.0, r, r, r));
  float d = filletUnion(filletUnion(top, stem, r), bottom, r);
  return min(d, cornerBox(mu, vec2(2.5, 2.5), vec2(140.0 / 210.0), vec4(47.0 / 210.0)));
}
`;

function compile(gl: WebGLRenderingContext, type: number, src: string) {
  const shader = gl.createShader(type);
  if (!shader) throw new Error("Unable to create shader");
  gl.shaderSource(shader, src);
  gl.compileShader(shader);
  if (!gl.getShaderParameter(shader, gl.COMPILE_STATUS)) {
    throw new Error(gl.getShaderInfoLog(shader) ?? "Shader compilation failed");
  }
  return shader;
}

/**
 * Compiles `frag` against the fullscreen vertex shader, binds a two-triangle
 * quad and makes the program current. Returns null (after logging) when the
 * context cannot build it, and a dispose function otherwise.
 */
export function createFieldProgram(gl: WebGLRenderingContext, frag: string, label: string) {
  let vs: WebGLShader;
  let fs: WebGLShader;
  try {
    vs = compile(gl, gl.VERTEX_SHADER, FIELD_VERT);
    fs = compile(gl, gl.FRAGMENT_SHADER, frag);
  } catch (error) {
    console.error(`[${label}]`, error);
    return null;
  }

  const program = gl.createProgram();
  if (!program) return null;
  gl.attachShader(program, vs);
  gl.attachShader(program, fs);
  gl.linkProgram(program);
  if (!gl.getProgramParameter(program, gl.LINK_STATUS)) return null;
  gl.useProgram(program);

  const buffer = gl.createBuffer();
  gl.bindBuffer(gl.ARRAY_BUFFER, buffer);
  gl.bufferData(gl.ARRAY_BUFFER, new Float32Array([-1, -1, 1, -1, -1, 1, -1, 1, 1, -1, 1, 1]), gl.STATIC_DRAW);
  const posLoc = gl.getAttribLocation(program, "position");
  gl.enableVertexAttribArray(posLoc);
  gl.vertexAttribPointer(posLoc, 2, gl.FLOAT, false, 0, 0);

  return {
    program,
    dispose() {
      gl.deleteBuffer(buffer);
      gl.deleteShader(vs);
      gl.deleteShader(fs);
      gl.deleteProgram(program);
    },
  };
}

/** The module's size in device pixels, the same in the hero and every seam. */
export function fieldCellSize(dpr: number) {
  return Math.max(12, Math.round(15 * dpr));
}
