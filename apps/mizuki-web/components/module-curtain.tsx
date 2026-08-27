'use client';

import Image from 'next/image';
import { useEffect, useState } from 'react';

// Entry curtain built from the logomark's module: a grid of rounded squares
// that clears diagonally along the brand ramp, so the page opens with the same
// unit the mark is made of.

const COLS = 14;

export function ModuleCurtain() {
  const [rows, setRows] = useState(0);
  const [state, setState] = useState<'holding' | 'clearing' | 'done'>('holding');

  useEffect(() => {
    if (window.matchMedia('(prefers-reduced-motion: reduce)').matches) {
      setState('done');
      return;
    }

    setRows(Math.ceil(window.innerHeight / (window.innerWidth / COLS)) + 1);
    const clear = window.setTimeout(() => setState('clearing'), 520);
    const finish = window.setTimeout(() => setState('done'), 1900);
    return () => {
      window.clearTimeout(clear);
      window.clearTimeout(finish);
    };
  }, []);

  useEffect(() => {
    document.documentElement.classList.toggle('is-loading', state !== 'done');
    return () => document.documentElement.classList.remove('is-loading');
  }, [state]);

  if (state === 'done') return null;

  return (
    <div
      className={`curtain ${state === 'clearing' ? 'is-clearing' : ''}`}
      style={{ ['--curtain-cols' as string]: COLS }}
      aria-hidden
    >
      <div className="curtain-grid">
        {Array.from({ length: COLS * rows }, (_, i) => {
          const col = i % COLS;
          const row = Math.floor(i / COLS);
          const delay = (col / COLS) * 0.55 + (row / Math.max(rows, 1)) * 0.35;
          return (
            <span
              key={i}
              className="curtain-cell"
              style={{ ['--cell-delay' as string]: `${delay.toFixed(3)}s` }}
            />
          );
        })}
      </div>
      <div className="curtain-brand">
        <Image src="/mizuki-mark.svg" alt="" width={1470} height={1050} priority />
      </div>
    </div>
  );
}
