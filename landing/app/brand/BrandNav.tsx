"use client";

import { useEffect, useRef, useState } from "react";
import Link from "next/link";
import { usePathname } from "next/navigation";
import { BRAND_ASSETS_ZIP, BRAND_SECTIONS } from "./_sections";

/**
 * The guidelines' navigation. From lg up, a sticky sidebar: every section, the
 * open section's parts (following the scroll), and the asset download. Below
 * lg, a sticky row of section tabs under the site header.
 */
export function BrandNav() {
  const pathname = usePathname();
  const active = BRAND_SECTIONS.find((s) => s.href === pathname) ?? BRAND_SECTIONS[0];
  const [part, setPart] = useState<string | null>(null);
  const tabs = useRef<HTMLUListElement>(null);

  // On narrow screens, bring the open section's tab to the middle of the row.
  useEffect(() => {
    const list = tabs.current;
    const tab = list?.querySelector<HTMLElement>('[aria-current="page"]');
    if (!list || !tab) return;
    list.scrollLeft = tab.offsetLeft - (list.clientWidth - tab.offsetWidth) / 2;
  }, [pathname]);

  // Follow the part nearest the top of the viewport.
  useEffect(() => {
    setPart(null);
    const els = [...document.querySelectorAll<HTMLElement>("[data-part]")];
    if (!els.length) return;
    const seen = new Map<string, number>();
    const observer = new IntersectionObserver(
      (entries) => {
        for (const entry of entries) {
          const id = (entry.target as HTMLElement).dataset.part ?? "";
          if (entry.isIntersecting) seen.set(id, entry.boundingClientRect.top);
          else seen.delete(id);
        }
        const first = [...seen.entries()].sort((a, b) => a[1] - b[1])[0];
        if (first) setPart(first[0]);
      },
      { rootMargin: "-20% 0px -55% 0px" },
    );
    els.forEach((el) => observer.observe(el));
    return () => observer.disconnect();
  }, [pathname]);

  return (
    <>
      {/* Narrow screens: a scrollable row of sections. */}
      <nav
        aria-label="Brand guidelines"
        className="sticky top-[calc(var(--header-h)+env(safe-area-inset-top))] z-30 -mx-5 border-b border-line-soft bg-ground/95 backdrop-blur-[14px] sm:-mx-8 lg:hidden"
      >
        <ul ref={tabs} className="no-scrollbar relative flex gap-1 overflow-x-auto px-5 py-2.5 sm:px-8">
          {BRAND_SECTIONS.map((s) => {
            const on = s.slug === active.slug;
            return (
              <li key={s.slug} className="shrink-0">
                <Link
                  href={s.href}
                  aria-current={on ? "page" : undefined}
                  className={`inline-flex items-center gap-2 rounded-full border px-3.5 py-2 font-mono text-[10px] uppercase tracking-[0.18em] transition-colors ${
                    on ? "border-line-strong bg-raised text-fg" : "border-transparent text-muted hover:text-fg"
                  }`}
                >
                  <span className="tabular-nums text-faint">{s.n}</span>
                  {s.title}
                </Link>
              </li>
            );
          })}
        </ul>
      </nav>

      {/* lg and up: the sidebar. */}
      <aside className="hidden lg:block">
        <nav
          aria-label="Brand guidelines"
          className="no-scrollbar sticky top-[calc(var(--header-h)+40px)] flex max-h-[calc(100dvh-var(--header-h)-64px)] flex-col overflow-y-auto pb-6 pt-12"
        >
          <Link href="/brand" className="font-mono text-[10px] font-medium uppercase tracking-[0.28em] text-muted transition-colors hover:text-fg">
            Brand guidelines
          </Link>
          <ul className="mt-6 flex flex-col gap-0.5">
            {BRAND_SECTIONS.map((s) => {
              const on = s.slug === active.slug;
              return (
                <li key={s.slug}>
                  <Link
                    href={s.href}
                    aria-current={on ? "page" : undefined}
                    className={`relative flex items-baseline gap-3 py-1.5 pl-3.5 font-mono text-[11.5px] tracking-[0.04em] transition-colors ${
                      on ? "text-fg" : "text-muted hover:text-fg"
                    }`}
                  >
                    <span
                      aria-hidden
                      className={`absolute left-0 top-1/2 h-4 w-[2px] -translate-y-1/2 rounded-full ${on ? "bg-ramp" : "bg-transparent"}`}
                    />
                    <span className="w-5 shrink-0 text-[10px] tabular-nums text-faint">{s.n}</span>
                    {s.title}
                  </Link>
                  {on && s.parts.length > 0 ? (
                    <ul className="mb-3 ml-[2.15rem] mt-1 flex flex-col border-l border-line-soft">
                      {s.parts.map((p) => (
                        <li key={p.id}>
                          <a
                            href={`#${p.id}`}
                            className={`-ml-px block border-l py-1 pl-3 text-[11px] leading-snug transition-colors ${
                              part === p.id ? "border-fg text-fg" : "border-transparent text-muted-2 hover:text-fg"
                            }`}
                          >
                            {p.title}
                          </a>
                        </li>
                      ))}
                    </ul>
                  ) : null}
                </li>
              );
            })}
          </ul>
          <a href={BRAND_ASSETS_ZIP} download className="btn btn-primary mt-8 w-full min-h-10 px-4">
            Brand assets <span aria-hidden="true">↓</span>
          </a>
          <p className="mt-3 text-[10.5px] leading-relaxed text-muted-2">Logos, icons, color tokens, social templates.</p>
        </nav>
      </aside>
    </>
  );
}
