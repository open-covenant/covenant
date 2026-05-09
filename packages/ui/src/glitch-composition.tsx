import type { CSSProperties } from 'react';

function hash(s: string): number {
  let h = 2166136261;
  for (let i = 0; i < s.length; i += 1) {
    h ^= s.charCodeAt(i);
    h = Math.imul(h, 16777619);
  }
  return Math.abs(h);
}

function mulberry32(seed: number) {
  let a = seed >>> 0;
  return () => {
    a = (a + 0x6d2b79f5) >>> 0;
    let t = a;
    t = Math.imul(t ^ (t >>> 15), t | 1);
    t ^= t + Math.imul(t ^ (t >>> 7), t | 61);
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}

type Props = {
  seed: number | string;
  className?: string;
  style?: CSSProperties;
};

export function GlitchComposition({ seed, className, style }: Props) {
  const n = typeof seed === 'string' ? hash(seed) : seed;
  const rand = mulberry32(n);
  const uid = `gc${n}`;
  const W = 400;
  const H = 180;

  const clusterX = W * 0.22;
  const clusterY = H * 0.18;
  const clusterW = W * 0.56;
  const clusterH = H * 0.64;
  const px = (t: number) => clusterX + t * clusterW;
  const py = (t: number) => clusterY + t * clusterH;

  const halftones = Array.from({ length: 3 + Math.floor(rand() * 2) }).map(() => ({
    x: px(rand() * 0.55),
    y: py(rand() * 0.55),
    w: 80 + rand() * 120,
    h: 40 + rand() * 70,
    density: 2 + rand() * 1.8,
    opacity: 0.55 + rand() * 0.3,
  }));

  const blackBlocks = Array.from({ length: 2 + Math.floor(rand() * 2) }).map(() => ({
    x: px(rand() * 0.65),
    y: py(rand() * 0.85),
    w: 50 + rand() * 70,
    h: 8 + rand() * 14,
  }));

  const limeBlocks = Array.from({ length: 1 + Math.floor(rand() * 2) }).map(() => ({
    x: px(rand() * 0.75),
    y: py(rand() * 0.8),
    w: 10 + rand() * 22,
    h: 3 + rand() * 6,
  }));

  const includeLimeDots = rand() > 0.55;
  const limeDotBlock = {
    x: px(rand() * 0.6),
    y: py(rand() * 0.7),
    w: 24 + rand() * 28,
    h: 6 + rand() * 10,
  };

  const plusMarks = Array.from({ length: 2 + Math.floor(rand() * 2) }).map(() => ({
    x: px(rand()),
    y: py(rand()),
    s: 4 + rand() * 3,
    color: rand() > 0.85 ? '#06f512' : '#111',
  }));

  const barcode = (() => {
    const x = px(rand() * 0.5);
    const y = py(rand() * 0.8);
    const totalW = 40 + rand() * 50;
    const h = 10 + rand() * 10;
    const bars: { x: number; w: number }[] = [];
    let cursor = 0;
    while (cursor < totalW) {
      const isBar = rand() > 0.35;
      const w = 0.6 + rand() * 2.2;
      if (isBar && cursor + w <= totalW) bars.push({ x: cursor, w });
      cursor += w + 0.4 + rand() * 1.2;
    }
    return { x, y, h, bars };
  })();

  function hexStr(min: number, max: number) {
    const chars = '0123456789ABCDEF';
    const len = min + Math.floor(rand() * (max - min + 1));
    let out = '';
    for (let i = 0; i < len; i += 1) out += chars[Math.floor(rand() * 16)];
    return out;
  }

  function digitStr(min: number, max: number) {
    const len = min + Math.floor(rand() * (max - min + 1));
    let out = '';
    for (let i = 0; i < len; i += 1) out += Math.floor(rand() * 10).toString();
    return out;
  }

  function binStr(min: number, max: number) {
    const len = min + Math.floor(rand() * (max - min + 1));
    let out = '';
    for (let i = 0; i < len; i += 1) {
      out += rand() > 0.5 ? '1' : '0';
      if ((i + 1) % 4 === 0 && i < len - 1) out += ' ';
    }
    return out;
  }

  const CYBER_TAGS = ['SYS', 'NODE', 'CH', 'SIG', 'BUF', 'TX', 'RX', 'SEQ', 'FRG', 'ERR', 'ACK', 'INIT', 'SCAN', 'LINK', 'HSH', 'KEY', 'PID', 'REV'];
  const CYBER_STATUS = ['OK', 'HOLD', 'LOCK', 'SYNC', 'DROP', 'FAIL', 'IDLE', 'PASS'];
  const pick = <T,>(arr: T[]): T => arr[Math.floor(rand() * arr.length)] as T;

  const numberLines = Array.from({ length: 3 + Math.floor(rand() * 3) }).map(() => {
    const styleRoll = rand();
    let text: string;
    if (styleRoll < 0.14) text = `SIG.${hexStr(4, 8)}`;
    else if (styleRoll < 0.28) text = `${pick(CYBER_TAGS)}.${hexStr(2, 4)}/${digitStr(2, 3)}`;
    else if (styleRoll < 0.4) text = `${pick(CYBER_TAGS)}[${pick(CYBER_STATUS)}]`;
    else if (styleRoll < 0.52) text = `T-${digitStr(2, 2)}:${digitStr(2, 2)}:${digitStr(2, 2)}`;
    else if (styleRoll < 0.62) text = `v${digitStr(1, 1)}.${digitStr(1, 2)}.${digitStr(1, 2)}-${rand() > 0.5 ? 'rc' : 'α'}.${digitStr(1, 2)}`;
    else if (styleRoll < 0.72) text = binStr(6, 12);
    else if (styleRoll < 0.82) text = `${pick(CYBER_TAGS)}//${digitStr(2, 3)}-${hexStr(2, 3)}`;
    else if (styleRoll < 0.9) text = `±${digitStr(1, 1)}.${digitStr(3, 5)}`;
    else text = `${digitStr(2, 3)}.${digitStr(3, 5)} / ${digitStr(3, 4)}`;
    return {
      x: px(rand() * 0.8),
      y: py(rand() * 0.95 + 0.02),
      text,
      size: 4.5 + rand() * 2.5,
      opacity: 0.55 + rand() * 0.35,
      color: rand() > 0.88 ? '#06f512' : '#111',
    };
  });

  return (
    <div aria-hidden="true" className={className} style={style}>
      <svg viewBox={`0 0 ${W} ${H}`} preserveAspectRatio="xMidYMid slice" className="block w-full h-full">
        <defs>
          <pattern id={`${uid}-dots-med`} x="0" y="0" width="3" height="3" patternUnits="userSpaceOnUse">
            <circle cx="1.5" cy="1.5" r="0.7" fill="#111" />
          </pattern>
          <pattern id={`${uid}-dots-sparse`} x="0" y="0" width="4" height="4" patternUnits="userSpaceOnUse">
            <circle cx="2" cy="2" r="0.55" fill="#111" />
          </pattern>
          <pattern id={`${uid}-dots-lime`} x="0" y="0" width="3" height="3" patternUnits="userSpaceOnUse">
            <circle cx="1.5" cy="1.5" r="0.85" fill="#06f512" />
          </pattern>
        </defs>

        {halftones.map((h, i) => (
          <rect key={`h-${i}`} x={h.x} y={h.y} width={h.w} height={h.h}
            fill={`url(#${uid}-${h.density > 3 ? 'dots-sparse' : 'dots-med'})`} opacity={h.opacity} />
        ))}
        {blackBlocks.map((b, i) => (
          <rect key={`k-${i}`} x={b.x} y={b.y} width={b.w} height={b.h} fill="#0b0b0b" />
        ))}
        {limeBlocks.map((b, i) => (
          <rect key={`l-${i}`} x={b.x} y={b.y} width={b.w} height={b.h} fill="#06f512" />
        ))}
        {includeLimeDots && (
          <rect x={limeDotBlock.x} y={limeDotBlock.y} width={limeDotBlock.w}
            height={limeDotBlock.h} fill={`url(#${uid}-dots-lime)`} />
        )}
        {plusMarks.map((p, i) => (
          <g key={`p-${i}`} stroke={p.color} strokeWidth="0.9">
            <line x1={p.x - p.s} y1={p.y} x2={p.x + p.s} y2={p.y} />
            <line x1={p.x} y1={p.y - p.s} x2={p.x} y2={p.y + p.s} />
          </g>
        ))}
        <g transform={`translate(${barcode.x} ${barcode.y})`} fill="#111">
          {barcode.bars.map((b, i) => (
            <rect key={`bc-${i}`} x={b.x} y={0} width={b.w} height={barcode.h} />
          ))}
        </g>
        {numberLines.map((nl, i) => (
          <text key={`n-${i}`} x={nl.x} y={nl.y}
            fontFamily="ui-monospace, SFMono-Regular, Menlo, monospace"
            fontSize={nl.size} fill={nl.color} opacity={nl.opacity}>
            {nl.text}
          </text>
        ))}
      </svg>
    </div>
  );
}
