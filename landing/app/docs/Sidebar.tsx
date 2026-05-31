"use client";

import Link from "next/link";
import { usePathname } from "next/navigation";
import { useState } from "react";
import { DOCS_NAV } from "./_nav";

export function Sidebar() {
  const pathname = usePathname();
  const [open, setOpen] = useState(false);
  // On the apex host the route is /docs/<slug>; on docs.* the middleware
  // rewrites the path to /docs/<slug> as well. `DOCS_NAV` declares hrefs
  // without the /docs prefix, so strip it before matching.
  const slug = pathname.replace(/^\/docs/, "") || "/";

  return (
    <>
      <button
        type="button"
        onClick={() => setOpen((v) => !v)}
        aria-label="Toggle navigation"
        className="fixed bottom-6 right-6 z-30 rounded-full border border-neutral-700 bg-neutral-900/90 px-4 py-2 text-xs uppercase tracking-widest text-neutral-300 backdrop-blur md:hidden"
      >
        {open ? "close" : "menu"}
      </button>

      <aside
        className={`fixed inset-y-0 left-0 z-20 w-72 overflow-y-auto border-r border-neutral-800/80 bg-[#030303] px-6 py-10 transition-transform duration-200 md:sticky md:top-0 md:h-screen md:translate-x-0 ${
          open ? "translate-x-0" : "-translate-x-full"
        }`}
      >
        <a
          href="https://opencovenant.org"
          className="mb-10 block text-xs uppercase tracking-[0.4em] text-neutral-500 hover:text-neutral-300"
        >
          ← covenant
        </a>

        <nav aria-label="Documentation" className="space-y-8 pb-20">
          {DOCS_NAV.map((section) => (
            <div key={section.title}>
              <h3 className="mb-3 text-[11px] font-medium uppercase tracking-[0.2em] text-neutral-500">
                {section.title}
              </h3>
              <ul className="space-y-1.5">
                {section.items.map((item) => {
                  const active = slug === item.href;
                  return (
                    <li key={item.href}>
                      <Link
                        href={item.href}
                        onClick={() => setOpen(false)}
                        className={`block border-l-2 pl-3 text-[13px] transition-colors ${
                          active
                            ? "border-neutral-100 text-neutral-50"
                            : "border-transparent text-neutral-400 hover:border-neutral-600 hover:text-neutral-200"
                        }`}
                      >
                        {item.label}
                      </Link>
                    </li>
                  );
                })}
              </ul>
            </div>
          ))}
        </nav>
      </aside>
    </>
  );
}
