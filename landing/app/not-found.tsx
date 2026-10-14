import type { Metadata } from "next";
import Link from "next/link";
import { SiteFooter } from "./SiteFooter";
import { SiteHeader } from "./SiteHeader";

export const metadata: Metadata = {
  title: "Page not found: Covenant",
  robots: { index: false, follow: false },
};

export default function NotFound() {
  return (
    <main
      id="main-content"
      className="relative flex min-h-[100dvh] flex-col bg-ground"
    >
      <SiteHeader />

      <div className="flex flex-1 flex-col items-center justify-center px-6 pb-16 pt-[calc(var(--header-h)+72px)] text-center">
        <p className="font-mono text-[11px] uppercase tracking-[0.4em] text-muted">
          404
        </p>
        <h1 className="page-title mt-4 max-w-4xl text-fg">
          This page could not be found
        </h1>
        <p className="mt-3 max-w-md text-sm leading-relaxed text-muted">
          The page you are looking for has moved or never existed. Head back to
          the start, browse the documentation, or get in touch.
        </p>
        <nav
          aria-label="Not found"
          className="mt-8 flex flex-wrap items-center justify-center gap-x-6 gap-y-2 text-[13px] text-muted"
        >
          <Link href="/" className="transition-colors hover:text-fg">
            Home
          </Link>
          <a
            href="https://docs.opencovenant.org"
            className="transition-colors hover:text-fg"
          >
            Documentation
          </a>
          <Link
            href="/roadmap"
            className="transition-colors hover:text-fg"
          >
            Roadmap
          </Link>
          <Link
            href="/contact"
            className="transition-colors hover:text-fg"
          >
            Contact
          </Link>
        </nav>
      </div>

      <SiteFooter className="pb-8" />
    </main>
  );
}
