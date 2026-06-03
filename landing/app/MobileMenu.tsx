"use client";

import { useEffect, useState } from "react";
import Link from "next/link";
import { Menu, X } from "lucide-react";
import { GithubIcon, XIcon } from "./_brand";

type NavItem = { label: string; href: string; external?: boolean };

const SOCIAL_ICONS: Record<string, React.ComponentType<{ className?: string }>> = {
  X: XIcon,
  GitHub: GithubIcon,
};

/**
 * Mobile nav: a hamburger that opens a drawer sliding in from the right edge,
 * dimming the page behind it. Backdrop tap or Escape closes.
 */
export function MobileMenu({
  items,
  socials,
}: {
  items: NavItem[];
  socials?: NavItem[];
}) {
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
      <button
        type="button"
        aria-label={open ? "Close menu" : "Open menu"}
        aria-expanded={open}
        aria-controls="mobile-menu"
        onClick={() => setOpen((v) => !v)}
        className="pointer-events-auto -mr-1 p-3 text-neutral-300 transition-colors hover:text-neutral-50"
      >
        {open ? <X className="h-5 w-5" /> : <Menu className="h-5 w-5" />}
      </button>

      {/* dim the page */}
      <div
        aria-hidden
        onClick={() => setOpen(false)}
        className={[
          "fixed inset-0 z-40 bg-black/60 transition-opacity duration-300",
          open ? "opacity-100" : "pointer-events-none opacity-0",
        ].join(" ")}
      />

      {/* drawer — slides in from the right */}
      <div
        id="mobile-menu"
        className={[
          "fixed right-0 top-0 z-50 flex h-full w-[78vw] max-w-[320px] flex-col border-l border-neutral-800/70 bg-[#0a0a0a]/95 backdrop-blur-md transition-transform duration-300 ease-out",
          open ? "translate-x-0" : "translate-x-full",
        ].join(" ")}
        style={{ paddingTop: "env(safe-area-inset-top)" }}
      >
        <div className="flex justify-end px-2 py-3">
          <button
            type="button"
            aria-label="Close menu"
            onClick={() => setOpen(false)}
            className="p-2 text-neutral-400 transition-colors hover:text-neutral-100"
          >
            <X className="h-5 w-5" />
          </button>
        </div>

        <nav aria-label="Primary" className="flex flex-col px-2">
          {items.map((item) =>
            item.external ? (
              <a
                key={item.href}
                href={item.href}
                target="_blank"
                rel="noopener noreferrer"
                onClick={() => setOpen(false)}
                className="px-4 py-3 text-[13px] uppercase tracking-[0.2em] text-neutral-300 transition-colors hover:text-neutral-50"
              >
                {item.label}
              </a>
            ) : (
              <Link
                key={item.href}
                href={item.href}
                onClick={() => setOpen(false)}
                className="px-4 py-3 text-[13px] uppercase tracking-[0.2em] text-neutral-300 transition-colors hover:text-neutral-50"
              >
                {item.label}
              </Link>
            ),
          )}
        </nav>

        {socials && socials.length > 0 && (
          <>
            <div className="mx-4 my-3 border-t border-neutral-800/60" />
            <div className="flex items-center gap-1 px-4">
              {socials.map((item) => {
                const Icon = SOCIAL_ICONS[item.label];
                return (
                  <a
                    key={item.href}
                    href={item.href}
                    target="_blank"
                    rel="noopener noreferrer"
                    aria-label={item.label}
                    onClick={() => setOpen(false)}
                    className="p-2 text-neutral-400 transition-colors hover:text-neutral-50"
                  >
                    {Icon ? <Icon className="h-4 w-4" /> : item.label}
                  </a>
                );
              })}
            </div>
          </>
        )}
      </div>
    </>
  );
}
