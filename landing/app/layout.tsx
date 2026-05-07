import type { Metadata } from "next";
import "./globals.css";

const SITE_URL = "https://opencovenant.org";
const SITE_NAME = "Covenant";
const TITLE = "Covenant — open agent-native operating layer";
const DESCRIPTION =
  "Covenant is the local-first coordination layer for agentic software. Intent, runtime, memory, identity, permissions, comms, compositor, and settlement — the OS-level primitives that humans and agents need to safely share a computer.";

export const metadata: Metadata = {
  metadataBase: new URL(SITE_URL),
  title: TITLE,
  description: DESCRIPTION,
  applicationName: SITE_NAME,
  alternates: { canonical: "/" },
  openGraph: {
    type: "website",
    url: SITE_URL,
    siteName: SITE_NAME,
    title: TITLE,
    description: DESCRIPTION,
  },
  twitter: {
    card: "summary_large_image",
    site: "@OpenCovenant",
    creator: "@OpenCovenant",
    title: TITLE,
    description: DESCRIPTION,
  },
};

export default function RootLayout({
  children,
}: {
  children: React.ReactNode;
}) {
  return (
    <html lang="en" className="dark">
      <body className="min-h-screen bg-[#030303] text-neutral-100 antialiased">
        {children}
      </body>
    </html>
  );
}
