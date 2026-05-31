import Link from "next/link";
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

      <HeroMesh src="/hero-bg.jpg" />

      <SiteHeader />

      <a
        href="https://sandbox.opencovenant.org"
        target="_blank"
        rel="noopener noreferrer"
        aria-label="Try the interactive Covenant sandbox"
        className="absolute left-1/2 top-1/2 z-10 -translate-x-1/2 translate-y-[calc(-50%+50px)] whitespace-nowrap rounded-full border border-neutral-500/40 bg-black/30 px-4 py-2.5 text-[11px] uppercase tracking-[0.28em] text-neutral-200 backdrop-blur-sm transition-colors hover:border-neutral-50/70 hover:text-neutral-50 sm:px-6 sm:text-[12px]"
      >
        Try the sandbox →
      </a>

      <SiteFooter
        className="absolute inset-x-0 z-20"
        style={{ bottom: "max(1.5rem, env(safe-area-inset-bottom))" }}
      />
    </main>
  );
}
