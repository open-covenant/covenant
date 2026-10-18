"use client";

// Live demos for the brand guidelines: the field resolving into the mark, the
// word reveal, the opening and a type tester. Each uses the site's own
// components or the exact values the site animates with.

import { useEffect, useRef, useState } from "react";
import { FieldCurtain } from "../ModuleCurtain";
import { ModuleField } from "../ModuleField";

const easeInOut = (t: number) => (t < 0.5 ? 4 * t * t * t : 1 - (-2 * t + 2) ** 3 / 2);

function useReducedMotion() {
  const [reduced, setReduced] = useState(false);
  useEffect(() => {
    const query = window.matchMedia("(prefers-reduced-motion: reduce)");
    const update = () => setReduced(query.matches);
    update();
    query.addEventListener("change", update);
    return () => query.removeEventListener("change", update);
  }, []);
  return reduced;
}

const replayButton =
  "inline-flex items-center gap-2 rounded-full border border-line-strong bg-ground/80 px-3.5 py-2 font-mono text-[10px] uppercase tracking-[0.18em] text-fg transition-colors hover:border-muted-2";

/**
 * The hero field in a frame. `auto` loops field → mark → field; `scrub` hands
 * the resolve to a slider.
 */
export function ResolveDemo({ mode = "auto", className = "" }: { mode?: "auto" | "scrub"; className?: string }) {
  const progress = useRef(mode === "scrub" ? 0.5 : 0);
  const [value, setValue] = useState(50);
  const reduced = useReducedMotion();

  useEffect(() => {
    if (mode !== "auto") return;
    if (reduced) {
      progress.current = 1;
      return;
    }
    let frame = 0;
    const start = performance.now();
    // 10s: drift, resolve over 2s, hold the mark, dissolve over 2s.
    const loop = (now: number) => {
      const t = ((now - start) / 1000) % 10;
      progress.current = t < 3 ? 0 : t < 5 ? easeInOut((t - 3) / 2) : t < 8 ? 1 : 1 - easeInOut((t - 8) / 2);
      frame = requestAnimationFrame(loop);
    };
    frame = requestAnimationFrame(loop);
    return () => cancelAnimationFrame(frame);
  }, [mode, reduced]);

  return (
    <div className={`relative ${className}`}>
      <ModuleField
        className="absolute inset-0 [&>canvas]:block [&>canvas]:h-full [&>canvas]:w-full"
        progress={progress}
        clearLeft={false}
      />
      {mode === "scrub" ? (
        <label className="absolute inset-x-4 bottom-4 flex items-center gap-4 rounded-full border border-line bg-ground/85 px-4 py-2.5 backdrop-blur-sm">
          <span className="font-mono text-[10px] uppercase tracking-[0.18em] text-muted">Field</span>
          <input
            type="range"
            min={0}
            max={100}
            value={value}
            onChange={(event) => {
              const v = Number(event.target.value);
              setValue(v);
              progress.current = v / 100;
            }}
            aria-label="Move from the field to the mark"
            className="brand-range flex-1"
          />
          <span className="font-mono text-[10px] uppercase tracking-[0.18em] text-muted">Mark</span>
        </label>
      ) : null}
    </div>
  );
}

/** A headline lifted in word by word, exactly as the site's section titles are. */
export function WordRevealDemo({ text }: { text: string }) {
  const [shown, setShown] = useState(false);
  const [run, setRun] = useState(0);
  const host = useRef<HTMLDivElement>(null);

  useEffect(() => {
    const el = host.current;
    if (!el) return;
    const observer = new IntersectionObserver(([entry]) => {
      if (entry?.isIntersecting) {
        setShown(true);
        observer.disconnect();
      }
    });
    observer.observe(el);
    return () => observer.disconnect();
  }, []);

  const replay = () => {
    setShown(false);
    setRun((r) => r + 1);
    requestAnimationFrame(() => requestAnimationFrame(() => setShown(true)));
  };

  return (
    <div ref={host} className="flex h-full flex-col justify-between gap-8 p-6 sm:p-8">
      <p key={run} className={`type-display text-[clamp(1.9rem,4.4vw,3.4rem)] text-fg ${shown ? "is-in" : ""}`}>
        {text.split(" ").map((word, i) => (
          <span key={`${word}-${i}`}>
            <span className="word-mask">
              <span className="word" style={{ ["--word-delay" as string]: `${i * 0.055}s` }}>
                {word}
              </span>
            </span>{" "}
          </span>
        ))}
      </p>
      <div className="flex items-center justify-between gap-4">
        <span className="font-mono text-[10px] uppercase tracking-[0.18em] text-muted-2">780ms · 55ms stagger</span>
        <button type="button" onClick={replay} className={replayButton}>
          Replay <span aria-hidden>↺</span>
        </button>
      </div>
    </div>
  );
}

/** The opening in miniature, over a page that has already loaded, landing on its own field. */
export function CurtainDemo() {
  const [run, setRun] = useState(0);
  const [inView, setInView] = useState(false);
  const host = useRef<HTMLDivElement>(null);
  const reduced = useReducedMotion();

  // Hold the cover until the demo is on screen, so it plays where it is seen.
  useEffect(() => {
    const el = host.current;
    if (!el) return;
    const observer = new IntersectionObserver(
      ([entry]) => {
        if (entry?.isIntersecting) {
          setInView(true);
          observer.disconnect();
        }
      },
      { threshold: 0.6 },
    );
    observer.observe(el);
    return () => observer.disconnect();
  }, []);

  return (
    <div ref={host} className="relative aspect-[16/9] w-full overflow-hidden">
      <div data-opening-field className="absolute inset-0">
        <ModuleField className="absolute inset-0 [&>canvas]:block [&>canvas]:h-full [&>canvas]:w-full" />
      </div>
      <div className="absolute inset-0 flex flex-col justify-end gap-3 p-6">
        <span className="font-mono text-[9px] uppercase tracking-[0.2em] text-muted">Covenant · open agent-native operating layer</span>
        <span className="type-display text-[clamp(1.4rem,3.2vw,2.4rem)] text-fg">An operating system</span>
      </div>
      {reduced ? null : (
        // A larger mark than the home page's (about 6% of the width), so it
        // reads at this size.
        <FieldCurtain
          key={run}
          play={inView}
          alignTo="[data-opening-field] canvas"
          clearLeft
          className="[--curtain-mark:9.9%]"
        />
      )}
      <button type="button" onClick={() => setRun((r) => r + 1)} className={`${replayButton} absolute bottom-4 right-4`}>
        Replay <span aria-hidden>↺</span>
      </button>
    </div>
  );
}

/** Type the brand's own words in either face, at any size. */
export function TypeTester() {
  const [text, setText] = useState("Permission, not trust");
  const [face, setFace] = useState<"display" | "mono">("display");
  const [size, setSize] = useState(64);

  const tab = (on: boolean) =>
    `rounded-full px-3.5 py-2 font-mono text-[10px] uppercase tracking-[0.18em] transition-colors ${
      on ? "bg-raised text-fg ring-1 ring-line-strong" : "text-muted hover:text-fg"
    }`;

  return (
    <div className="flex flex-col">
      <div className="flex flex-wrap items-center gap-3 border-b border-line px-4 py-3">
        <div className="flex gap-1" role="group" aria-label="Typeface">
          <button type="button" className={tab(face === "display")} aria-pressed={face === "display"} onClick={() => setFace("display")}>
            Archivo Expanded
          </button>
          <button type="button" className={tab(face === "mono")} aria-pressed={face === "mono"} onClick={() => setFace("mono")}>
            JetBrains Mono
          </button>
        </div>
        <label className="ml-auto flex items-center gap-3 font-mono text-[10px] uppercase tracking-[0.18em] text-muted">
          Size
          <input
            type="range"
            min={14}
            max={128}
            value={size}
            onChange={(event) => setSize(Number(event.target.value))}
            className="brand-range w-32"
            aria-label="Font size"
          />
          <span className="w-10 text-right tabular-nums text-fg-soft">{size}px</span>
        </label>
      </div>
      <label className="sr-only" htmlFor="type-tester">
        Sample text
      </label>
      <textarea
        id="type-tester"
        value={text}
        onChange={(event) => setText(event.target.value)}
        rows={2}
        spellCheck={false}
        className={`min-h-[12rem] w-full resize-y bg-transparent px-5 py-6 text-fg placeholder:text-faint focus-visible:outline-offset-[-4px] ${
          face === "display" ? "type-display" : "font-mono font-medium"
        }`}
        style={{ fontSize: size, lineHeight: face === "display" ? 0.92 : 1.3 }}
        placeholder="Type something"
      />
    </div>
  );
}
