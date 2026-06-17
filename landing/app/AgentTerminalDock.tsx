"use client";

import { useEffect, useState } from "react";
import { X } from "lucide-react";
import { AgentTerminal } from "./AgentTerminal";

/**
 * Positions the agent terminal: docked on the right at xl, a slide-in/out
 * drawer below xl. One AgentTerminal instance — responsive classes flip it
 * from off-canvas drawer to fixed dock, so the stream only runs once.
 */
export function AgentTerminalDock() {
  const [open, setOpen] = useState(false);

  useEffect(() => {
    if (!open) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") setOpen(false);
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [open]);

  return (
    <>
      <div
        aria-hidden
        onClick={() => setOpen(false)}
        className={[
          "fixed inset-0 z-20 bg-black/50 transition-opacity duration-300 xl:hidden",
          open ? "opacity-100" : "pointer-events-none opacity-0",
        ].join(" ")}
      />

      <div
        className={[
          "fixed right-0 bottom-0 top-16 z-30 w-[88vw] max-w-[380px] shadow-[0_8px_40px_-12px_rgba(0,0,0,0.85)] transition-transform duration-300 ease-out",
          open ? "translate-x-0" : "translate-x-full",
          "xl:absolute xl:left-auto xl:right-6 xl:top-[92px] xl:bottom-3 xl:z-20 xl:w-[min(38vw,440px)] xl:max-w-none xl:translate-x-0 xl:transition-none",
        ].join(" ")}
      >
        <button
          type="button"
          onClick={() => setOpen((v) => !v)}
          aria-label={open ? "Hide agent log" : "Show agent log"}
          aria-expanded={open}
          className="pointer-events-auto absolute left-0 top-36 flex -translate-x-full -translate-y-1/2 items-center gap-1.5 rounded-l-md border border-r-0 border-neutral-800/70 bg-[#050505]/90 px-2 py-4 text-neutral-400 backdrop-blur-md transition-colors hover:text-neutral-100 sm:top-80 xl:hidden"
        >
          {open ? (
            <X className="h-4 w-4" />
          ) : (
            <svg
              className="h-4 w-4"
              viewBox="0 0 24 24"
              fill="none"
              stroke="currentColor"
              strokeWidth="2"
              strokeLinecap="round"
              strokeLinejoin="round"
              aria-hidden="true"
            >
              <polyline points="4 17 10 11 4 5" />
            </svg>
          )}
          <span className="whitespace-nowrap text-[9px] uppercase tracking-[0.18em] [writing-mode:vertical-rl]">
            build log
          </span>
        </button>

        <AgentTerminal
          className="h-full w-full border-l border-neutral-800/70 xl:rounded-md xl:border"
          onClose={() => setOpen(false)}
        />
      </div>
    </>
  );
}
