import Image from "next/image";
import Link from "next/link";
import { HeroMesh } from "./HeroMesh";
import { MobileMenu } from "./MobileMenu";
import { PixelReveal } from "./PixelReveal";
import { GithubIcon, GITHUB_URL, NAV_LINKS, XIcon, X_URL } from "./_brand";

const RELEASE_DATE = "ALPHA TARGET: 13.05.2026";

export default function Page() {
  return (
    <main className="relative h-[100dvh] min-h-[100svh] overflow-hidden bg-[#030303]">
      <HeroMesh src="/hero-bg.jpg" />

      <header
        className="pointer-events-none absolute inset-x-0 top-0 z-20 flex justify-center pt-[max(35px,env(safe-area-inset-top))] sm:pt-[max(59px,env(safe-area-inset-top))]"
      >
        <Image
          src="/logo.svg"
          alt="covenant"
          width={255}
          height={54}
          priority
          className="pointer-events-auto h-[42px] w-auto opacity-95 sm:h-[60px]"
        />
      </header>

      <div
        className="absolute left-2 z-20 sm:left-8 sm:top-10"
        style={{ top: "max(0.5rem, env(safe-area-inset-top))" }}
      >
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

      <nav
        className="absolute right-2 z-20 flex items-center gap-1 sm:right-8 sm:top-10 sm:gap-3"
        style={{ top: "max(0.5rem, env(safe-area-inset-top))" }}
      >
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

      <section className="relative z-10 flex h-full flex-col items-center justify-center px-4 text-center">
        <div className="pointer-events-none h-[min(80vh,90vw)] w-[min(80vh,90vw)] max-w-none">
          <PixelReveal src="/hero-bg.jpg" stagger={720} fadeDur={280} />
        </div>
      </section>

      <p className="absolute left-1/2 top-[80%] z-10 -translate-x-1/2 px-4 text-[12px] tracking-[0.4em] text-neutral-400 sm:text-[14px]">
        {RELEASE_DATE}
      </p>

      <footer
        className="absolute inset-x-0 z-20 flex justify-center px-4 text-center text-[10px] tracking-widest text-neutral-500 uppercase sm:text-[11px]"
        style={{ bottom: "max(1.5rem, env(safe-area-inset-bottom))" }}
      >
        open agent-native operating layer
      </footer>
    </main>
  );
}
