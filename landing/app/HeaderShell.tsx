"use client";

import { useEffect, useState } from "react";

/**
 * The fixed header bar: a near-opaque night ground that gains a hairline once
 * the page scrolls (`.site-header` in globals.css).
 */
export function HeaderShell({ children }: { children: React.ReactNode }) {
  const [scrolled, setScrolled] = useState(false);

  useEffect(() => {
    const update = () => setScrolled(window.scrollY > 40);
    update();
    window.addEventListener("scroll", update, { passive: true });
    return () => window.removeEventListener("scroll", update);
  }, []);

  return (
    <header
      className="site-header fixed inset-x-0 top-0 z-40"
      data-scrolled={scrolled}
      style={{ paddingTop: "env(safe-area-inset-top)" }}
    >
      {children}
    </header>
  );
}
