"use client";

import { useEffect, useRef, useState } from "react";
import { StateMark } from "./_marks";
import { TRACKS } from "./_roadmap";

/**
 * The roadmap's navigation, in the brand guidelines' pattern. From lg up, a
 * sticky sidebar of tracks with their state and size, following the scroll.
 * Below lg, a sticky row of track tabs under the site header.
 */
export function RoadmapNav() {
  const [active, setActive] = useState<string>(TRACKS[0].id);
  const tabs = useRef<HTMLUListElement>(null);

  // Follow the track nearest the top of the viewport.
  useEffect(() => {
    const els = [...document.querySelectorAll<HTMLElement>("[data-track]")];
    if (!els.length) return;
    const seen = new Map<string, number>();
    const observer = new IntersectionObserver(
      (entries) => {
        for (const entry of entries) {
          const id = (entry.target as HTMLElement).dataset.track ?? "";
          if (entry.isIntersecting) seen.set(id, entry.boundingClientRect.top);
          else seen.delete(id);
        }
        const first = [...seen.entries()].sort((a, b) => a[1] - b[1])[0];
        if (first) setActive(first[0]);
      },
      { rootMargin: "-20% 0px -55% 0px" },
    );
    els.forEach((el) => observer.observe(el));
    return () => observer.disconnect();
  }, []);

  // On narrow screens, keep the active tab in view.
  useEffect(() => {
    const list = tabs.current;
    const tab = list?.querySelector<HTMLElement>('[aria-current="true"]');
    if (!list || !tab) return;
    list.scrollTo({ left: tab.offsetLeft - (list.clientWidth - tab.offsetWidth) / 2, behavior: "smooth" });
  }, [active]);

  return (
    <>
      <nav
        aria-label="Roadmap tracks"
        className="sticky top-[calc(var(--header-h)+env(safe-area-inset-top))] z-30 -mx-5 border-b border-line-soft bg-ground/95 backdrop-blur-[14px] sm:-mx-8 lg:hidden"
      >
        <ul ref={tabs} className="no-scrollbar flex gap-1 overflow-x-auto px-5 py-2.5 sm:px-8">
          {TRACKS.map((t) => {
            const on = t.id === active;
            return (
              <li key={t.id} className="shrink-0">
                <a
                  href={`#${t.id}`}
                  aria-current={on ? "true" : undefined}
                  className={`inline-flex items-center gap-2 rounded-full border px-3.5 py-2 font-mono text-[10px] uppercase tracking-[0.18em] transition-colors ${
                    on ? "border-line-strong bg-raised text-fg" : "border-transparent text-muted hover:text-fg"
                  }`}
                >
                  <StateMark state={t.state} size="sm" />
                  {t.code ? <span className="tabular-nums text-faint">{t.code}</span> : null}
                  {t.title}
                </a>
              </li>
            );
          })}
        </ul>
      </nav>

      <aside className="hidden lg:block">
        <nav
          aria-label="Roadmap tracks"
          className="no-scrollbar sticky top-[calc(var(--header-h)+40px)] flex max-h-[calc(100dvh-var(--header-h)-64px)] flex-col overflow-y-auto pb-6 pt-16"
        >
          <a
            href="#top"
            className="font-mono text-[10px] font-medium uppercase tracking-[0.28em] text-muted transition-colors hover:text-fg"
          >
            Roadmap
          </a>
          <ul className="mt-6 flex flex-col gap-0.5">
            {TRACKS.map((t) => {
              const on = t.id === active;
              return (
                <li key={t.id}>
                  <a
                    href={`#${t.id}`}
                    aria-current={on ? "true" : undefined}
                    className={`relative flex items-baseline gap-3 py-2 pl-3.5 font-mono text-[11.5px] leading-snug tracking-[0.04em] transition-colors ${
                      on ? "text-fg" : "text-muted hover:text-fg"
                    }`}
                  >
                    <span
                      aria-hidden
                      className={`absolute left-0 top-1/2 h-4 w-[2px] -translate-y-1/2 rounded-full ${on ? "bg-ramp" : "bg-transparent"}`}
                    />
                    <StateMark state={t.state} size="sm" className="translate-y-[-1px]" />
                    <span className="w-5 shrink-0 text-[10px] tabular-nums text-faint">{t.code ?? ""}</span>
                    <span className="min-w-0 flex-1">{t.title}</span>
                    <span className="text-[10px] tabular-nums text-faint">{t.items.length}</span>
                  </a>
                </li>
              );
            })}
          </ul>
        </nav>
      </aside>
    </>
  );
}
