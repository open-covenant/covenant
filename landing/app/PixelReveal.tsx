"use client";

import { useEffect, useRef } from "react";

type Cell = { col: number; row: number; startMs: number };

export function PixelReveal({
  src,
  cellSize = 14,
  stagger = 900,
  fadeDur = 320,
}: {
  src: string;
  cellSize?: number;
  stagger?: number;
  fadeDur?: number;
}) {
  const canvasRef = useRef<HTMLCanvasElement>(null);

  useEffect(() => {
    const canvas = canvasRef.current;
    if (!canvas) return;
    const host = canvas.parentElement;
    if (!host) return;
    const ctx = canvas.getContext("2d");
    if (!ctx) return;

    let cancelled = false;
    let frame = 0;
    let t0 = 0;
    let cells: Cell[] = [];
    const reduced =
      typeof window !== "undefined" &&
      window.matchMedia?.("(prefers-reduced-motion: reduce)").matches;

    const img = new Image();
    img.decoding = "async";
    img.src = src;

    function size() {
      const rect = host!.getBoundingClientRect();
      if (rect.width === 0 || rect.height === 0) return rect;
      const dpr = window.devicePixelRatio || 1;
      canvas!.width = Math.max(1, Math.floor(rect.width * dpr));
      canvas!.height = Math.max(1, Math.floor(rect.height * dpr));
      ctx!.setTransform(dpr, 0, 0, dpr, 0, 0);
      const cols = Math.ceil(rect.width / cellSize);
      const rows = Math.ceil(rect.height / cellSize);
      const next: Cell[] = [];
      for (let r = 0; r < rows; r += 1) {
        for (let c = 0; c < cols; c += 1) {
          next.push({ col: c, row: r, startMs: Math.random() * stagger });
        }
      }
      cells = next;
      return rect;
    }

    function drawFull() {
      const rect = host!.getBoundingClientRect();
      ctx!.clearRect(0, 0, rect.width, rect.height);
      ctx!.drawImage(img, 0, 0, rect.width, rect.height);
    }

    function drawCell(cell: Cell, alpha: number, rect: DOMRect) {
      const x = cell.col * cellSize;
      const y = cell.row * cellSize;
      const sx = (x / rect.width) * img.naturalWidth;
      const sy = (y / rect.height) * img.naturalHeight;
      const sw = Math.min(
        (cellSize / rect.width) * img.naturalWidth,
        img.naturalWidth - sx,
      );
      const sh = Math.min(
        (cellSize / rect.height) * img.naturalHeight,
        img.naturalHeight - sy,
      );
      if (sw <= 0 || sh <= 0) return;
      ctx!.globalAlpha = alpha;
      ctx!.drawImage(img, sx, sy, sw, sh, x, y, cellSize, cellSize);
    }

    function render(now: number) {
      if (cancelled) return;
      const rect = host!.getBoundingClientRect();
      ctx!.clearRect(0, 0, rect.width, rect.height);
      const elapsed = now - t0;
      let allDone = true;
      for (const cell of cells) {
        const t = elapsed - cell.startMs;
        const alpha = t <= 0 ? 0 : t >= fadeDur ? 1 : t / fadeDur;
        if (alpha < 1) allDone = false;
        if (alpha > 0) drawCell(cell, alpha, rect);
      }
      ctx!.globalAlpha = 1;
      if (allDone) {
        drawFull();
        frame = 0;
        return;
      }
      frame = requestAnimationFrame(render);
    }

    function start() {
      if (cancelled) return;
      const rect = size();
      if (rect.width === 0 || rect.height === 0) return;
      if (reduced || cells.length === 0) {
        drawFull();
        return;
      }
      t0 = performance.now();
      frame = requestAnimationFrame(render);
    }

    if (img.complete && img.naturalWidth > 0) {
      start();
    } else {
      img.addEventListener("load", start, { once: true });
    }

    const observer = new ResizeObserver(() => {
      if (cancelled || !img.complete || !img.naturalWidth) return;
      size();
      if (frame === 0) drawFull();
    });
    observer.observe(host);

    return () => {
      cancelled = true;
      observer.disconnect();
      if (frame) cancelAnimationFrame(frame);
    };
  }, [src, cellSize, stagger, fadeDur]);

  return (
    <canvas
      ref={canvasRef}
      aria-hidden
      style={{ width: "100%", height: "100%", display: "block" }}
    />
  );
}
