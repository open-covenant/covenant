"use client";

import { useEffect, useRef } from "react";

type Cell = { col: number; row: number; startMs: number };

export function PixelReveal({
  src,
  cellSize = 8,
  stagger = 1500,
  fadeDur = 280,
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

    function coverFit(rect: DOMRect) {
      // Intrinsic size, centered. Mesh samples with the same projection so
      // glyphs line up with the underlying figure.
      const scale = 1;
      const drawW = img.naturalWidth;
      const drawH = img.naturalHeight;
      const offsetX = (rect.width - drawW) / 2;
      const offsetY = (rect.height - drawH) / 2;
      return { scale, drawW, drawH, offsetX, offsetY };
    }

    function drawFull() {
      const rect = host!.getBoundingClientRect();
      ctx!.clearRect(0, 0, rect.width, rect.height);
      const fit = coverFit(rect);
      ctx!.drawImage(img, fit.offsetX, fit.offsetY, fit.drawW, fit.drawH);
    }

    function drawCell(cell: Cell, alpha: number, rect: DOMRect) {
      const fit = coverFit(rect);
      const x = cell.col * cellSize;
      const y = cell.row * cellSize;
      const sx = (x - fit.offsetX) / fit.scale;
      const sy = (y - fit.offsetY) / fit.scale;
      const sw = cellSize / fit.scale;
      const sh = cellSize / fit.scale;
      // Outside the cover rect: source is clamped by canvas API; nothing
      // useful to draw, skip to avoid edge-pixel smearing.
      if (sx + sw <= 0 || sy + sh <= 0) return;
      if (sx >= img.naturalWidth || sy >= img.naturalHeight) return;
      const clampedSx = Math.max(0, sx);
      const clampedSy = Math.max(0, sy);
      const clampedSw = Math.min(sw - (clampedSx - sx), img.naturalWidth - clampedSx);
      const clampedSh = Math.min(sh - (clampedSy - sy), img.naturalHeight - clampedSy);
      if (clampedSw <= 0 || clampedSh <= 0) return;
      const dx = x + (clampedSx - sx) * fit.scale;
      const dy = y + (clampedSy - sy) * fit.scale;
      const dw = clampedSw * fit.scale;
      const dh = clampedSh * fit.scale;
      ctx!.globalAlpha = alpha;
      ctx!.drawImage(img, clampedSx, clampedSy, clampedSw, clampedSh, dx, dy, dw, dh);
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
