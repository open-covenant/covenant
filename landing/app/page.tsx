import Link from "next/link";
import { AgentTerminalDock } from "./AgentTerminalDock";
import { HeroMesh } from "./HeroMesh";
import { SiteFooter } from "./SiteFooter";
import { SiteHeader } from "./SiteHeader";

export default function Page() {
  return (
    <main
      id="main-content"
      className="relative flex min-h-[100svh] flex-col overflow-x-hidden bg-[#030303] sm:block sm:h-[100dvh] sm:overflow-hidden"
    >
      <link
        rel="preload"
        as="image"
        href="/hero-bg.jpg"
        type="image/jpeg"
        fetchPriority="high"
      />

      <div className="sr-only">
        <h1>Covenant: open agent-native operating layer</h1>
        <p>
          Covenant is the open, local-first coordination layer for agentic
          software. It gives humans and agents eight host-level primitives
          (intent, runtime, memory, identity, permissions, communication, a
          compositor, and on-chain settlement) so they can safely share one
          computer. Try the{" "}
          <a href="https://sandbox.opencovenant.org">interactive sandbox</a>,
          read the{" "}
          <a href="https://docs.opencovenant.org/concepts">
            documentation on the eight primitives
          </a>
          , review the <Link href="/roadmap">development roadmap</Link>, or read
          the{" "}
          <a href="https://doi.org/10.5281/zenodo.20134416">
            technical whitepaper
          </a>
          .
        </p>
      </div>

      <SiteHeader />

      <AgentTerminalDock />

      <div className="relative z-10 flex flex-1 flex-col items-center justify-start gap-3 px-6 pb-8 pt-24 text-center sm:absolute sm:inset-0 sm:flex-none sm:justify-center sm:gap-6 sm:pb-0 sm:pt-0 xl:pr-[31rem]">
        <div className="relative aspect-[1168/774] w-full overflow-hidden sm:aspect-auto sm:h-[53vh]">
          <HeroMesh src="/hero-bg.jpg" />
        </div>
        <div className="flex max-w-2xl flex-col gap-3 sm:gap-4">
          <h2 className="text-balance text-[1.6rem] font-extralight uppercase leading-[1.15] tracking-[1px] text-white sm:text-[2.2rem] sm:leading-[1.1] sm:tracking-[2px]">
            An operating system that builds itself
          </h2>
          <p className="text-balance text-[15px] font-light leading-relaxed text-neutral-200 sm:text-lg">
          Not a chatbot. Not an agent framework. An open operating layer where
          every agent runs under a <span className="text-white">signed grant</span>,
          every action leaves a <span className="text-white">receipt</span>, and the
          system itself is built in the open by the{" "}
          <span className="text-white">autonomous infrastructure it provides</span>.
          </p>
        </div>
        <div className="flex flex-wrap items-center justify-center gap-3">
          <a
            href="https://sandbox.opencovenant.org"
            target="_blank"
            rel="noopener noreferrer"
            aria-label="Try the interactive Covenant sandbox"
            className="rounded-full bg-white px-5 py-2.5 text-[11px] uppercase tracking-[0.28em] text-black transition-colors hover:bg-neutral-200 sm:px-6 sm:text-[12px]"
          >
            Try the sandbox →
          </a>
          <Link
            href="/about"
            className="rounded-full border border-neutral-700/50 px-5 py-2.5 text-[11px] uppercase tracking-[0.28em] text-neutral-400 transition-colors hover:border-neutral-500 hover:text-neutral-100 sm:px-6 sm:text-[12px]"
          >
            Learn about Covenant
          </Link>
        </div>
      </div>

      <SiteFooter className="relative z-20 w-full shrink-0 pb-[max(1rem,env(safe-area-inset-bottom))] pt-2 sm:absolute sm:inset-x-0 sm:bottom-[max(1.5rem,env(safe-area-inset-bottom))] sm:w-auto sm:pb-0 sm:pt-0 xl:pr-[31rem]" />
    </main>
  );
}
