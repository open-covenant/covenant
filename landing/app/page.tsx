import Image from "next/image";
import Link from "next/link";
import { HeroMesh } from "./HeroMesh";
import { MobileMenu } from "./MobileMenu";
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
          className="pointer-events-auto h-[48px] w-auto opacity-95 sm:h-[69px]"
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


      <a
        href="https://sandbox.opencovenant.org"
        target="_blank"
        rel="noopener noreferrer"
        className="absolute left-1/2 top-[80%] z-10 -translate-x-1/2 rounded-full border border-neutral-500/40 bg-black/30 px-6 py-2.5 text-[11px] uppercase tracking-[0.28em] text-neutral-200 backdrop-blur-sm transition-colors hover:border-neutral-50/70 hover:text-neutral-50 sm:text-[12px]"
      >
        Try the sandbox →
      </a>

      <footer
        className="absolute inset-x-0 z-20 flex items-center justify-center gap-2.5 px-4 text-center text-[10px] tracking-widest text-neutral-500 uppercase sm:text-[11px]"
        style={{ bottom: "max(1.5rem, env(safe-area-inset-bottom))" }}
      >
        <Image
          src="/logomark.svg"
          alt="covenant"
          width={30}
          height={15}
          className="h-auto w-[30px] opacity-70"
        />
        <span>Covenant  ·  Open infrastructure for agent-native computing  ·  {RELEASE_DATE.toLowerCase()}</span>
      </footer>
    </main>
  );
}
