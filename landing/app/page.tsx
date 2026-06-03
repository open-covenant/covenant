import Link from "next/link";
import { AgentTerminalDock } from "./AgentTerminalDock";
import { HeroMesh } from "./HeroMesh";
import { SiteFooter } from "./SiteFooter";
import { SiteHeader } from "./SiteHeader";

export default function Page() {
  return (
    <main
      id="main-content"
      className="relative h-[100dvh] min-h-[100svh] overflow-hidden bg-[#030303]"
    >
      <link
        rel="preload"
        as="image"
        href="/hero-bg.jpg"
        type="image/jpeg"
        fetchPriority="high"
      />

      <div className="sr-only">
        <h1>Covenant — open agent-native operating layer</h1>
        <p>
          Covenant is the open, local-first coordination layer for agentic
          software. It gives humans and agents eight host-level primitives —
          intent, runtime, memory, identity, permissions, communication, a
          compositor, and on-chain settlement — so they can safely share one
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

      <div className="absolute inset-0 z-10 flex -translate-y-[100px] flex-col items-center justify-center gap-6 px-6 pb-6 pt-[calc(88px_+_env(safe-area-inset-top))] text-center">
        <div className="relative w-full max-h-[53vh] flex-1 overflow-hidden">
          <HeroMesh src="/hero-bg.jpg" />
        </div>
        <p className="max-w-2xl text-balance text-[15px] font-light leading-relaxed text-neutral-200 sm:text-lg">
          Not a chatbot. Not an agent framework. An open operating layer where
          every agent runs under a <span className="text-white">signed grant</span>,
          every action leaves a <span className="text-white">receipt</span>, and the
          system itself is built in the open by an{" "}
          <span className="text-white">autonomous loop that never stops</span> —
          live, in public, onchain.
        </p>
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

      <SiteFooter
        className="absolute inset-x-0 z-20"
        style={{ bottom: "max(1.5rem, env(safe-area-inset-bottom))" }}
      />
    </main>
  );
}
