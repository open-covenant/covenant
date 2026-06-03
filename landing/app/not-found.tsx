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
      className="relative flex min-h-[100dvh] flex-col bg-[#030303]"
    >
      <SiteHeader />

      <div className="flex flex-1 flex-col items-center justify-center px-6 text-center">
        <p className="font-mono text-[11px] uppercase tracking-[0.4em] text-neutral-400">
          404
        </p>
        <h1 className="mt-4 text-2xl font-extralight tracking-tight text-neutral-100 sm:text-3xl">
          This page could not be found
        </h1>
        <p className="mt-3 max-w-md text-sm leading-relaxed text-neutral-400">
          The page you are looking for has moved or never existed. Head back to
          the start, browse the documentation, or get in touch.
        </p>
        <nav
          aria-label="Not found"
          className="mt-8 flex flex-wrap items-center justify-center gap-x-6 gap-y-2 text-[13px] text-neutral-400"
        >
          <Link href="/" className="transition-colors hover:text-neutral-50">
            Home
          </Link>
          <a
            href="https://docs.opencovenant.org"
            className="transition-colors hover:text-neutral-50"
          >
            Documentation
          </a>
          <Link
            href="/roadmap"
            className="transition-colors hover:text-neutral-50"
          >
            Roadmap
          </Link>
          <Link
            href="/contact"
            className="transition-colors hover:text-neutral-50"
          >
            Contact
          </Link>
        </nav>
      </div>

      <SiteFooter className="pb-8" />
    </main>
  );
}
