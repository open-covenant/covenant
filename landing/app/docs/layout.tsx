import type { Metadata } from "next";
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
    images: [
      {
        url: "https://opencovenant.org/opengraph-image.jpg",
        width: 1200,
        height: 630,
        alt: "Covenant: open agent-native operating layer",
      },
    ],
  },
  twitter: {
    card: "summary_large_image",
    site: "@OpenCovenant",
    creator: "@OpenCovenant",
    images: "https://opencovenant.org/opengraph-image.jpg",
  },
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

        <main id="main-content" className="min-w-0 flex-1 px-6 py-12 md:px-12 md:py-16 lg:px-20">
          <article className="prose-docs mx-auto w-full max-w-[760px]">
            {children}
          </article>

          <SiteFooter className="mx-auto mt-24 max-w-[760px] border-t border-neutral-800/80 pt-6" />
        </main>
      </div>
    </div>
  );
}
