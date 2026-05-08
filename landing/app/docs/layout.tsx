import type { Metadata } from "next";
import { Sidebar } from "./Sidebar";

export const metadata: Metadata = {
  metadataBase: new URL("https://docs.opencovenant.org"),
  title: {
    default: "Documentation — Covenant",
    template: "%s — Covenant docs",
  },
  description:
    "Reference, concepts, and operational guides for Covenant — the open, agent-native operating layer.",
  alternates: { canonical: "/" },
};

export default function DocsLayout({
  children,
}: {
  children: React.ReactNode;
}) {
  return (
    <div className="min-h-[100dvh] bg-[#030303] text-neutral-200">
      <div className="mx-auto flex w-full max-w-[1400px]">
        <Sidebar />

        <main className="min-w-0 flex-1 px-6 py-12 md:px-12 md:py-16 lg:px-20">
          <article className="prose-docs mx-auto w-full max-w-[760px]">
            {children}
          </article>

          <footer className="mx-auto mt-24 flex max-w-[760px] items-center justify-between border-t border-neutral-800/80 pt-6 text-[11px] uppercase tracking-widest text-neutral-500">
            <span>Apache 2.0 · open-covenant/covenant</span>
            <a
              href="https://github.com/open-covenant/covenant"
              target="_blank"
              rel="noopener noreferrer"
              className="hover:text-neutral-200"
            >
              github ↗
            </a>
          </footer>
        </main>
      </div>
    </div>
  );
}
