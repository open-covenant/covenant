import Image from "next/image";
import Link from "next/link";
import { MobileMenu } from "./MobileMenu";
import { GithubIcon, GITHUB_URL, NAV_LINKS, XIcon, X_URL } from "./_brand";

/**
 * Site-wide header: centered logo, left nav, right socials. Single source
 * of truth — home and every top-level subpage mount this so the logo size,
 * position, and nav never drift. The three pieces are absolutely positioned
 * so the logo stays centered regardless of nav width; the side groups share
 * the logo's top offset + height so their contents sit on the logo's center.
 */
export function SiteHeader() {
  return (
    <>
      <header className="pointer-events-none absolute inset-x-0 top-0 z-20 flex justify-center pt-[max(18px,env(safe-area-inset-top))] sm:pt-[max(28px,env(safe-area-inset-top))]">
        <Link href="/" className="pointer-events-auto" aria-label="Covenant home">
          <Image
            src="/logo.svg"
            alt="covenant"
            width={255}
            height={54}
            priority
            className="h-[30px] w-auto opacity-95 sm:h-[42px]"
          />
        </Link>
      </header>

      <div className="absolute left-2 z-20 flex h-[30px] items-center top-[max(18px,env(safe-area-inset-top))] sm:left-8 sm:h-[42px] sm:top-[max(28px,env(safe-area-inset-top))]">
        <div className="sm:hidden">
          <MobileMenu items={NAV_LINKS} />
        </div>
        <nav className="hidden items-center gap-3 sm:flex">
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

      <nav className="absolute right-2 z-20 flex h-[30px] items-center gap-1 top-[max(18px,env(safe-area-inset-top))] sm:right-8 sm:h-[42px] sm:top-[max(28px,env(safe-area-inset-top))] sm:gap-3">
        <a
          href={X_URL}
          target="_blank"
          rel="noopener noreferrer"
          aria-label="Covenant on X"
          className="p-3 text-neutral-400 transition-colors hover:text-neutral-50"
        >
          <XIcon className="h-5 w-5" />
        </a>
        <a
          href={GITHUB_URL}
          target="_blank"
          rel="noopener noreferrer"
          aria-label="Covenant on GitHub"
          className="p-3 text-neutral-400 transition-colors hover:text-neutral-50"
        >
          <GithubIcon className="h-5 w-5" />
        </a>
      </nav>
    </>
  );
}
