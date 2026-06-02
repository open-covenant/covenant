import Image from "next/image";
import Link from "next/link";
import { HeaderStats } from "./HeaderStats";
import { MobileMenu } from "./MobileMenu";
import {
  GithubIcon,
  GITHUB_URL,
  NAV_LINKS,
  SOCIAL_LINKS,
  XIcon,
  X_URL,
} from "./_brand";

/**
 * Site-wide header bar: left-aligned logo followed by the nav, with the live
 * stats and socials on the right, and a bottom border separating it from page
 * content (the home terminal docks directly beneath it). Single source of
 * truth — home and every top-level subpage mount this. Below xl the nav and
 * socials fold into the mobile drawer.
 */
export function SiteHeader() {
  return (
    <header
      className="absolute inset-x-0 top-0 z-20 border-b border-neutral-800/70 bg-[#030303]/72 backdrop-blur-md"
      style={{ paddingTop: "env(safe-area-inset-top)" }}
    >
      <div className="flex h-16 items-center justify-between gap-4 px-3 sm:h-20 sm:px-8">
        <div className="flex min-w-0 items-center gap-3 sm:gap-6">
          <Link href="/" aria-label="Covenant home" className="shrink-0">
            <Image
              src="/logo.svg"
              alt="Covenant"
              width={255}
              height={54}
              priority
              className="h-[30px] w-auto opacity-95 sm:h-[42px]"
            />
          </Link>

          <div className="xl:hidden">
            <MobileMenu items={NAV_LINKS} socials={SOCIAL_LINKS} />
          </div>

          <nav aria-label="Primary" className="hidden items-center gap-2 xl:flex">
            {NAV_LINKS.map((item) =>
              item.external ? (
                <a
                  key={item.href}
                  href={item.href}
                  target="_blank"
                  rel="noopener noreferrer"
                  className="px-3 py-3 text-[12px] uppercase tracking-[0.2em] text-neutral-400 transition-colors hover:text-neutral-50"
                >
                  {item.label}
                </a>
              ) : (
                <Link
                  key={item.href}
                  href={item.href}
                  className="px-3 py-3 text-[12px] uppercase tracking-[0.2em] text-neutral-400 transition-colors hover:text-neutral-50"
                >
                  {item.label}
                </Link>
              ),
            )}
          </nav>
        </div>

        <div className="hidden shrink-0 items-center gap-4 xl:flex">
          <HeaderStats />
          <span aria-hidden className="h-4 w-px bg-neutral-700/50" />
          <a
            href={X_URL}
            target="_blank"
            rel="noopener noreferrer"
            aria-label="Covenant on X"
            className="p-2 text-neutral-400 transition-colors hover:text-neutral-50"
          >
            <XIcon className="h-4 w-4" />
          </a>
          <a
            href={GITHUB_URL}
            target="_blank"
            rel="noopener noreferrer"
            aria-label="Covenant on GitHub"
            className="p-2 text-neutral-400 transition-colors hover:text-neutral-50"
          >
            <GithubIcon className="h-4 w-4" />
          </a>
        </div>
      </div>
    </header>
  );
}
