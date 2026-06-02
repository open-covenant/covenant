"use client";

import { useEffect, useRef, useState } from "react";
import Link from "next/link";
import { Menu, X } from "lucide-react";
import { GithubIcon, XIcon } from "./_brand";

type NavItem = { label: string; href: string; external?: boolean };

const SOCIAL_ICONS: Record<string, React.ComponentType<{ className?: string }>> = {
  X: XIcon,
  GitHub: GithubIcon,
};

export function MobileMenu({
  items,
  socials,
}: {
  items: NavItem[];
  socials?: NavItem[];
}) {
  const [open, setOpen] = useState(false);
  const ref = useRef<HTMLDivElement>(null);

  useEffect(() => {
    if (!open) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") setOpen(false);
    };
    const onPointer = (e: PointerEvent) => {
      if (ref.current && !ref.current.contains(e.target as Node)) {
        setOpen(false);
      }
    };
    window.addEventListener("keydown", onKey);
    window.addEventListener("pointerdown", onPointer);
    return () => {
      window.removeEventListener("keydown", onKey);
      window.removeEventListener("pointerdown", onPointer);
    };
  }, [open]);

  return (
    <div ref={ref} className="relative">
      <button
        type="button"
        aria-label={open ? "Close menu" : "Open menu"}
        aria-expanded={open}
        aria-controls="mobile-menu"
        onClick={() => setOpen((v) => !v)}
        className="pointer-events-auto p-3 text-neutral-400 transition-colors hover:text-neutral-50"
      >
        {open ? <X className="h-5 w-5" /> : <Menu className="h-5 w-5" />}
      </button>
      {open && (
        <div
          id="mobile-menu"
          className="pointer-events-auto absolute left-2 top-full mt-1 min-w-[160px] rounded-md border border-neutral-800/60 bg-[#0a0a0a]/95 py-2 backdrop-blur-sm"
        >
          {items.map((item) =>
            item.external ? (
              <a
                key={item.href}
                href={item.href}
                target="_blank"
                rel="noopener noreferrer"
                onClick={() => setOpen(false)}
                className="block px-4 py-2 text-[13px] uppercase tracking-[0.2em] text-neutral-400 transition-colors hover:text-neutral-50"
              >
                {item.label}
              </a>
            ) : (
              <Link
                key={item.href}
                href={item.href}
                onClick={() => setOpen(false)}
                className="block px-4 py-2 text-[13px] uppercase tracking-[0.2em] text-neutral-400 transition-colors hover:text-neutral-50"
              >
                {item.label}
              </Link>
            ),
          )}
          {socials && socials.length > 0 && (
            <>
              <div className="my-2 border-t border-neutral-800/60" />
              <div className="flex items-center gap-1 px-2">
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
      )}
    </div>
  );
}
