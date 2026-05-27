import { HeroMesh } from "./HeroMesh";
import { SiteFooter } from "./SiteFooter";
import { SiteHeader } from "./SiteHeader";

export default function Page() {
  return (
    <main className="relative h-[100dvh] min-h-[100svh] overflow-hidden bg-[#030303]">
      <HeroMesh src="/hero-bg.jpg" />

      <SiteHeader />

      <a
        href="https://sandbox.opencovenant.org"
        target="_blank"
        rel="noopener noreferrer"
        className="absolute left-1/2 top-[80%] z-10 -translate-x-1/2 rounded-full border border-neutral-500/40 bg-black/30 px-6 py-2.5 text-[11px] uppercase tracking-[0.28em] text-neutral-200 backdrop-blur-sm transition-colors hover:border-neutral-50/70 hover:text-neutral-50 sm:text-[12px]"
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
