import type { Metadata } from "next";
import { CARD_SIZE, cardAlt, cardImageUrl } from "../_metadata";
import { SiteFooter } from "../SiteFooter";
import { Sidebar } from "./Sidebar";

export const metadata: Metadata = {
  metadataBase: new URL("https://docs.opencovenant.org"),
  title: {
    default: "Documentation: Covenant",
    template: "%s: Covenant docs",
  },
  description:
    "Reference, concepts, and operational guides for Covenant, the open, agent-native operating layer.",
  // No canonical here: each docs page declares its own absolute canonical via buildDocsMetadata.
  openGraph: {
    type: "website",
    siteName: "Covenant docs",
    locale: "en_US",
    images: [{ url: cardImageUrl("/docs"), ...CARD_SIZE, alt: cardAlt("Documentation") }],
  },
  twitter: {
    card: "summary_large_image",
    site: "@OpenCovenant",
    creator: "@OpenCovenant",
    images: cardImageUrl("/docs"),
  },
};

export default function DocsLayout({
  children,
}: {
  children: React.ReactNode;
}) {
  return (
    <div className="min-h-[100dvh] bg-ground text-fg-2">
      <div className="mx-auto flex w-full max-w-[1400px]">
        <Sidebar />

        <main id="main-content" className="min-w-0 flex-1 px-6 py-12 md:px-12 md:py-16 lg:px-20">
          <article className="prose-docs mx-auto w-full max-w-[760px]">
            {children}
          </article>

          <SiteFooter variant="minimal" className="mx-auto mt-24 max-w-[760px] border-t border-line/80 pt-6" />
        </main>
      </div>
    </div>
  );
}
