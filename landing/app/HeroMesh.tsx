"use client";

import { useEffect, useRef } from "react";

const CHARS = "0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZ?#@$%&*+-=/<>";

function MeshCanvas({ backgroundImageSrc }: { backgroundImageSrc: string }) {
  const canvasRef = useRef<HTMLCanvasElement>(null);
  const animFrameRef = useRef<number>(0);

  useEffect(() => {
    const canvas = canvasRef.current;
    if (!canvas) return;
    const canvasEl = canvas;
    const container = canvas.parentElement;
    if (!container) return;
    const host = container;

    const context = canvasEl.getContext("2d");
    if (!context) return;
    const ctx: CanvasRenderingContext2D = context;

    const MOUSE_RADIUS = 200;
    const MOUSE_MAX_PUSH = 24;
    const MOUSE_SPRING = 0.03;
    const MOUSE_PUSH_STRENGTH = 0.012;

    let mouseX = -9999;
    let mouseY = -9999;

    const onMouseMove = (e: MouseEvent) => {
      const rect = host.getBoundingClientRect();
      mouseX = e.clientX - rect.left;
      mouseY = e.clientY - rect.top;
    };
    const onMouseLeave = () => {
      mouseX = -9999;
      mouseY = -9999;
    };

    const onTouch = (e: TouchEvent) => {
      const touch = e.touches[0];
      if (!touch) return;
      const rect = host.getBoundingClientRect();
      mouseX = touch.clientX - rect.left;
      mouseY = touch.clientY - rect.top;
    };
    const onTouchEnd = () => {
      mouseX = -9999;
      mouseY = -9999;
    };

    const onDocOut = (e: MouseEvent) => {
      if (!e.relatedTarget) onMouseLeave();
    };
    window.addEventListener("mousemove", onMouseMove);
    document.addEventListener("mouseout", onDocOut);
    window.addEventListener("touchstart", onTouch, { passive: true });
    window.addEventListener("touchmove", onTouch, { passive: true });
    window.addEventListener("touchend", onTouchEnd, { passive: true });
    window.addEventListener("touchcancel", onTouchEnd, { passive: true });

    let glyphs: Array<{
      char: string;
      contourX: number;
      contourY: number;
      darkness: number;
      detail: number;
      displaceX: number;
      displaceY: number;
      flowScale: number;
      opacityBase: number;
      phase: number;
      sprite: HTMLCanvasElement;
      threshold: number;
      x: number;
      y: number;
    }> = [];
    let imageSample: {
      data: Uint8ClampedArray;
      height: number;
      width: number;
    } | null = null;
    let fontPx = 8;
    const spriteCache = new Map<string, HTMLCanvasElement>();
    const sampleCanvas = document.createElement("canvas");
    const sampleContext = sampleCanvas.getContext("2d");

    function getSprite(char: string, size: number) {
      const key = `${char}:${size}`;
      const cached = spriteCache.get(key);
      if (cached) return cached;

      const sprite = document.createElement("canvas");
      sprite.width = Math.ceil(size * 0.72);
      sprite.height = size + 1;
      const sctx = sprite.getContext("2d");
      if (!sctx) return sprite;

      sctx.fillStyle = "#ffffff";
      sctx.font = `${size}px ui-monospace, "SF Mono", Menlo, Consolas, monospace`;
      sctx.textBaseline = "top";
      sctx.fillText(char, 0, 0);

      spriteCache.set(key, sprite);
      return sprite;
    }

    function pickChar(seed: number, luminance: number, detail: number): string {
      const mix = Math.abs(
        Math.floor(seed * 7 + luminance * 73 + detail * 41 + seed * detail * 11),
      );
      return CHARS[mix % CHARS.length];
    }

    function sampleLuminance(x: number, y: number) {
      if (!imageSample) return 0.5;
      const clampedX = Math.max(
        0,
        Math.min(imageSample.width - 1, Math.round(x)),
      );
      const clampedY = Math.max(
        0,
        Math.min(imageSample.height - 1, Math.round(y)),
      );
      const index = (clampedY * imageSample.width + clampedX) * 4;
      const red = imageSample.data[index] / 255;
      const green = imageSample.data[index + 1] / 255;
      const blue = imageSample.data[index + 2] / 255;
      return red * 0.2126 + green * 0.7152 + blue * 0.0722;
    }

    function sampleImageInfo(
      containerX: number,
      containerY: number,
      rect: DOMRect,
    ) {
      if (!imageSample) {
        return { contourX: 1, contourY: 0, detail: 0.3, luminance: 0.5 };
      }

      const scale = Math.max(
        rect.width / imageSample.width,
        rect.height / imageSample.height,
      );
      const drawWidth = imageSample.width * scale;
      const drawHeight = imageSample.height * scale;
      const offsetX = (rect.width - drawWidth) / 2;
      const offsetY = (rect.height - drawHeight) / 2;
      const imageX = (containerX - offsetX) / scale;
      const imageY = (containerY - offsetY) / scale;

      const luminance = sampleLuminance(imageX, imageY);
      const left = sampleLuminance(imageX - 1.2, imageY);
      const right = sampleLuminance(imageX + 1.2, imageY);
      const top = sampleLuminance(imageX, imageY - 1.2);
      const bottom = sampleLuminance(imageX, imageY + 1.2);
      const signedGradientX = right - left;
      const signedGradientY = bottom - top;
      const gradientX = Math.abs(signedGradientX);
      const gradientY = Math.abs(signedGradientY);
      const diagonal = Math.abs(
        sampleLuminance(imageX + 1, imageY + 1) -
          sampleLuminance(imageX - 1, imageY - 1),
      );
      const edge = Math.min(1, (gradientX + gradientY + diagonal * 0.7) * 1.8);
      const contrast = Math.abs(luminance - 0.5) * 2;
      const contourLength = Math.hypot(signedGradientX, signedGradientY);
      const contourX =
        contourLength > 0.0001 ? -signedGradientY / contourLength : 1;
      const contourY =
        contourLength > 0.0001 ? signedGradientX / contourLength : 0;

      return {
        contourX,
        contourY,
        detail: Math.max(0.08, Math.min(1, edge * 0.78 + contrast * 0.34)),
        luminance,
      };
    }

    function resizeCanvas() {
      const rect = host.getBoundingClientRect();
      if (rect.width === 0 || rect.height === 0) return;
      const dpr = window.devicePixelRatio || 1;
      canvasEl.width = rect.width * dpr;
      canvasEl.height = rect.height * dpr;
      ctx.setTransform(dpr, 0, 0, dpr, 0, 0);

      fontPx = 12;
      const cellSize = 16;
      const columns = Math.ceil(rect.width / cellSize) + 3;
      const rows = Math.ceil(rect.height / cellSize) + 3;

      glyphs = [];
      for (let row = 0; row < rows; row += 1) {
        for (let column = 0; column < columns; column += 1) {
          const seed = (column * 17 + row * 31) % 11;
          const x = column * cellSize + cellSize * 0.16;
          const y = row * cellSize + cellSize * 0.14;
          const info = sampleImageInfo(x, y, rect);
          const detail = info.detail;
          const luminance = info.luminance;
          const darkness = 1 - luminance;
          const shadowPenalty = Math.max(0, darkness - 0.48) / 0.52;
          const shadowFade = Math.min(
            1,
            Math.pow(shadowPenalty, 1.22) * 1.32,
          );
          const char = pickChar(seed + column * 3 + row * 5, luminance, detail);
          glyphs.push({
            char,
            contourX: info.contourX,
            contourY: info.contourY,
            darkness,
            detail,
            displaceX: 0,
            displaceY: 0,
            flowScale: 0.42 + detail * 1.55,
            opacityBase:
              0.058 +
              detail * (0.24 + (1 - shadowFade) * 0.14) +
              (seed % 4) * 0.012,
            phase: column * 0.37 + row * 0.21 + seed * 0.16,
            sprite: getSprite(char, fontPx),
            threshold:
              0.022 +
              (1 - detail) * 0.04 +
              luminance * 0.014 +
              shadowFade * 0.16,
            x,
            y,
          });
        }
      }
    }

    const resizeObserver = new ResizeObserver(() => {
      resizeCanvas();
    });
    resizeObserver.observe(host);
    window.addEventListener("resize", resizeCanvas);
    resizeCanvas();

    const posterImage = new Image();
    posterImage.src = backgroundImageSrc;
    posterImage.decoding = "async";
    posterImage.onload = () => {
      if (!sampleContext) return;
      sampleCanvas.width = posterImage.naturalWidth;
      sampleCanvas.height = posterImage.naturalHeight;
      sampleContext.drawImage(posterImage, 0, 0);
      const pixels = sampleContext.getImageData(
        0,
        0,
        posterImage.naturalWidth,
        posterImage.naturalHeight,
      );
      imageSample = {
        data: pixels.data,
        height: pixels.height,
        width: pixels.width,
      };
      resizeCanvas();
    };

    function render(time: number) {
      const rect = host.getBoundingClientRect();
      const now = time * 0.001;
      const breath = Math.sin(now * 0.2) * 0.5 + 0.5;
      ctx.clearRect(0, 0, rect.width, rect.height);
      ctx.fillStyle = "rgba(3, 3, 3, 0.12)";
      ctx.fillRect(0, 0, rect.width, rect.height);

      const mouseActive = mouseX > -9000;

      for (const glyph of glyphs) {
        if (mouseActive) {
          const dx = glyph.x - mouseX;
          const dy = glyph.y - mouseY;
          const dist = Math.sqrt(dx * dx + dy * dy);
          if (dist < MOUSE_RADIUS && dist > 0.1) {
            const strength =
              ((MOUSE_RADIUS - dist) / MOUSE_RADIUS) * MOUSE_PUSH_STRENGTH;
            const radialX = dx / dist;
            const radialY = dy / dist;

            const edgeWeight = Math.min(1, glyph.detail * 1.6);
            const dot = radialX * glyph.contourX + radialY * glyph.contourY;
            const sign = dot >= 0 ? 1 : -1;
            const contourPushX = glyph.contourX * sign;
            const contourPushY = glyph.contourY * sign;

            const contourBlend = edgeWeight * 0.92;
            const pushX =
              radialX * (1 - contourBlend) + contourPushX * contourBlend;
            const pushY =
              radialY * (1 - contourBlend) + contourPushY * contourBlend;
            const pushLen = Math.sqrt(pushX * pushX + pushY * pushY) || 1;

            const edgeBoost = 1 + edgeWeight * 1.8;
            let targetX =
              glyph.displaceX +
              (pushX / pushLen) * strength * MOUSE_RADIUS * edgeBoost;
            let targetY =
              glyph.displaceY +
              (pushY / pushLen) * strength * MOUSE_RADIUS * edgeBoost;
            const maxPush = MOUSE_MAX_PUSH * (0.3 + edgeWeight * 0.7);
            const targetDist = Math.sqrt(
              targetX * targetX + targetY * targetY,
            );
            if (targetDist > maxPush) {
              targetX = (targetX / targetDist) * maxPush;
              targetY = (targetY / targetDist) * maxPush;
            }
            glyph.displaceX = targetX;
            glyph.displaceY = targetY;
            if (Math.random() < 0.004 + edgeWeight * 0.018) {
              glyph.char = CHARS[Math.floor(Math.random() * CHARS.length)];
              glyph.sprite = getSprite(glyph.char, fontPx);
            }
          } else {
            glyph.displaceX *= 1 - MOUSE_SPRING;
            glyph.displaceY *= 1 - MOUSE_SPRING;
          }
        } else {
          glyph.displaceX *= 1 - MOUSE_SPRING;
          glyph.displaceY *= 1 - MOUSE_SPRING;
        }

        const shimmer =
          Math.sin(glyph.x * 0.028 + now * 0.62 + glyph.phase) * 0.36 +
          Math.cos(glyph.y * 0.024 - now * 0.36 + glyph.phase) * 0.3 +
          Math.sin(
            (glyph.x + glyph.y) * 0.016 - now * 0.24 + glyph.phase,
          ) * 0.26;
        const cluster =
          Math.sin(now * 0.36 + glyph.y * 0.012 + glyph.phase) * 0.075 +
          Math.cos(now * 0.28 - glyph.x * 0.01 + glyph.phase * 0.7) * 0.065;
        const pulse =
          Math.sin(now * 0.5 + glyph.phase * 1.3 + glyph.y * 0.01) * 0.04 +
          breath * 0.024;
        const contourGlow =
          Math.sin(
            now * 0.34 +
              glyph.phase +
              glyph.x * glyph.contourX * 0.02 +
              glyph.y * glyph.contourY * 0.02,
          ) *
            0.06 +
          glyph.detail * 0.05;
        const intensity = Math.max(
          0,
          Math.min(
            1,
            glyph.opacityBase + shimmer * 0.1 + cluster + pulse + contourGlow,
          ),
        );
        if (intensity < glyph.threshold) continue;

        const shadowSuppression = Math.max(0, glyph.darkness - 0.48) / 0.52;
        const shadowFade = Math.min(1, Math.pow(shadowSuppression, 1.24) * 1.34);
        const shadowGain = 1 - shadowFade * 0.94;
        const opacity =
          0.016 +
          shadowGain * 0.036 +
          intensity *
            shadowGain *
            (0.24 + glyph.detail * 0.16 + (1 - shadowFade) * 0.14);
        const streamX =
          Math.sin(now * 0.26 + glyph.y * 0.018 + glyph.phase) * 1.12 +
          Math.cos(now * 0.2 - glyph.x * 0.013 + glyph.phase) * 0.9;
        const streamY =
          Math.cos(now * 0.24 + glyph.x * 0.015 + glyph.phase) * 0.94 +
          Math.sin(now * 0.18 - glyph.y * 0.011 + glyph.phase * 1.2) * 0.78;
        const swirlX =
          Math.sin(
            now * 0.14 + glyph.x * 0.009 - glyph.y * 0.007 + glyph.phase,
          ) *
            1.24 +
          Math.cos(now * 0.1 + glyph.y * 0.012 + glyph.phase * 0.8) * 0.82;
        const swirlY =
          Math.cos(
            now * 0.13 + glyph.y * 0.01 - glyph.x * 0.008 + glyph.phase,
          ) *
            1.14 -
          Math.sin(now * 0.1 + glyph.x * 0.011 + glyph.phase * 0.9) * 0.76;
        const curl =
          Math.sin(
            now * 0.31 + (glyph.x - glyph.y) * 0.01 + glyph.phase,
          ) *
            0.52 +
          Math.cos(
            now * 0.23 + (glyph.x + glyph.y) * 0.008 + glyph.phase,
          ) *
            0.4;
        const contourWave =
          Math.sin(
            now * 0.4 +
              glyph.phase * 1.1 +
              glyph.x * glyph.contourX * 0.018 +
              glyph.y * glyph.contourY * 0.018,
          ) *
            1.62 +
          Math.cos(
            now * 0.25 -
              glyph.phase * 0.8 +
              glyph.x * glyph.contourY * 0.012 -
              glyph.y * glyph.contourX * 0.012,
          ) *
            1.04;
        const contourPulse =
          Math.cos(
            now * 0.21 +
              glyph.phase +
              glyph.x * glyph.contourY * 0.01 -
              glyph.y * glyph.contourX * 0.01,
          ) * 0.58;
        const contourWeight = 0.3 + glyph.detail * 1.15;
        const contourDriftX =
          glyph.contourX * contourWave * glyph.flowScale * contourWeight -
          glyph.contourY * contourPulse * glyph.flowScale * 0.32;
        const contourDriftY =
          glyph.contourY * contourWave * glyph.flowScale * contourWeight +
          glyph.contourX * contourPulse * glyph.flowScale * 0.32;
        const ambientWeight = 0.9 - glyph.detail * 0.45;
        const driftX =
          ((streamX * 0.6 + swirlX * 0.4 + curl * 0.7) * ambientWeight +
            contourDriftX) *
          (0.85 + breath * 0.35);
        const driftY =
          ((streamY * 0.58 + swirlY * 0.42 - curl * 0.55) * ambientWeight +
            contourDriftY) *
          (0.85 + breath * 0.35);

        const mx = glyph.displaceX;
        const my = glyph.displaceY;
        const disturbance = Math.min(
          1,
          Math.sqrt(mx * mx + my * my) / MOUSE_MAX_PUSH,
        );
        const hoverBoost = 1 + disturbance * 2.4;

        ctx.globalAlpha = Math.min(1, opacity * hoverBoost);
        ctx.drawImage(
          glyph.sprite,
          glyph.x + driftX + mx,
          glyph.y + driftY + my,
        );
        ctx.globalAlpha = Math.min(
          1,
          opacity * (0.4 + glyph.detail * 0.18) * hoverBoost,
        );
        ctx.drawImage(
          glyph.sprite,
          glyph.x + driftX * 1.52 + mx * 0.7,
          glyph.y + driftY * 1.52 + my * 0.7,
        );
        ctx.globalAlpha = opacity * 0.3;
        ctx.drawImage(
          glyph.sprite,
          glyph.x - driftX * 0.82 + mx * 0.3,
          glyph.y - driftY * 0.82 + my * 0.3,
        );
      }
      ctx.globalAlpha = 1;

      animFrameRef.current = requestAnimationFrame(render);
    }

    animFrameRef.current = requestAnimationFrame(render);

    return () => {
      resizeObserver.disconnect();
      window.removeEventListener("resize", resizeCanvas);
      window.removeEventListener("mousemove", onMouseMove);
      document.removeEventListener("mouseout", onDocOut);
      window.removeEventListener("touchstart", onTouch);
      window.removeEventListener("touchmove", onTouch);
      window.removeEventListener("touchend", onTouchEnd);
      window.removeEventListener("touchcancel", onTouchEnd);
      if (animFrameRef.current) {
        cancelAnimationFrame(animFrameRef.current);
      }
    };
  }, [backgroundImageSrc]);

  return (
    <canvas
      ref={canvasRef}
      style={{
        position: "absolute",
        inset: 0,
        width: "100%",
        height: "100%",
        display: "block",
        zIndex: 1,
        pointerEvents: "none",
      }}
    />
  );
}

export function HeroMesh({ src }: { src: string }) {
  return (
    <div className="pointer-events-none absolute inset-0 z-0 overflow-hidden">
      <MeshCanvas backgroundImageSrc={src} />
    </div>
  );
}
