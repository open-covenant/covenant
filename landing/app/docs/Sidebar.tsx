"use client";

import Link from "next/link";
import { usePathname } from "next/navigation";
import { useState } from "react";
import { CovenantLockup } from "../_brand";
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
        className="btn btn-primary fixed bottom-6 right-6 z-30 min-h-0 px-5 py-2.5 md:hidden"
      >
        {open ? "close" : "menu"}
      </button>

      <aside
        className={`fixed inset-y-0 left-0 z-20 w-72 overflow-y-auto border-r border-line/80 bg-ground px-6 py-10 transition-transform duration-200 md:sticky md:top-0 md:h-screen md:translate-x-0 ${
          open ? "translate-x-0" : "-translate-x-full"
        }`}
      >
        <a
          href="https://opencovenant.org"
          aria-label="Covenant home"
          className="mb-10 inline-flex text-fg transition-opacity hover:opacity-75"
        >
          <CovenantLockup markClassName="h-7 w-auto" />
        </a>

        <nav aria-label="Documentation" className="space-y-8 pb-20">
          {DOCS_NAV.map((section) => (
            <div key={section.title}>
              <h3 className="mb-3 text-[11px] font-medium uppercase tracking-[0.2em] text-muted">
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
                            ? "border-fg text-fg"
                            : "border-transparent text-muted hover:border-faint hover:text-fg-2"
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
