import type { Metadata, Viewport } from "next";
import "./globals.css";

const SITE_URL = "https://opencovenant.org";
const SITE_NAME = "Covenant";
const TITLE = "Covenant — open agent-native operating layer";
const DESCRIPTION =
  "Covenant is the coordination layer for agentic software. Intent, runtime, memory, identity, permissions, comms, and settlement are host-level primitives for humans and agents.";

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
    images: [
      {
        url: "/opengraph-image.jpg",
        width: 1200,
        height: 630,
        alt: "Covenant — open agent-native operating layer",
      },
    ],
  },
  twitter: {
    card: "summary_large_image",
    site: "@OpenCovenant",
    creator: "@OpenCovenant",
    title: TITLE,
    description: DESCRIPTION,
    images: "/twitter-image.jpg",
  },
};

export const viewport: Viewport = {
  width: "device-width",
  initialScale: 1,
  themeColor: "#030303",
  viewportFit: "cover",
};

function SiteJsonLd() {
  const graph = {
    "@context": "https://schema.org",
    "@graph": [
      {
        "@type": "Organization",
        "@id": `${SITE_URL}/#org`,
        name: SITE_NAME,
        url: SITE_URL,
        logo: `${SITE_URL}/logomark.svg`,
        description: DESCRIPTION,
        sameAs: [
          "https://x.com/OpenCovenant",
          "https://github.com/open-covenant/covenant",
        ],
        contactPoint: {
          "@type": "ContactPoint",
          contactType: "security",
          email: "security@opencovenant.org",
          url: `${SITE_URL}/contact`,
        },
      },
      {
        "@type": "WebSite",
        "@id": `${SITE_URL}/#website`,
        name: SITE_NAME,
        url: SITE_URL,
        description: DESCRIPTION,
        inLanguage: "en",
        publisher: { "@id": `${SITE_URL}/#org` },
      },
      {
        "@type": "SoftwareApplication",
        "@id": `${SITE_URL}/#software`,
        name: SITE_NAME,
        description: DESCRIPTION,
        url: SITE_URL,
        applicationCategory: "DeveloperApplication",
        operatingSystem: "Linux, macOS",
        codeRepository: "https://github.com/open-covenant/covenant",
        programmingLanguage: "Rust",
        license: "https://www.apache.org/licenses/LICENSE-2.0",
        downloadUrl: "https://github.com/open-covenant/covenant/releases",
        author: { "@id": `${SITE_URL}/#org` },
        publisher: { "@id": `${SITE_URL}/#org` },
        offers: { "@type": "Offer", price: "0", priceCurrency: "USD" },
      },
    ],
  };
  return (
    <script
      type="application/ld+json"
      dangerouslySetInnerHTML={{ __html: JSON.stringify(graph) }}
    />
  );
}

export default function RootLayout({
  children,
}: {
  children: React.ReactNode;
}) {
  return (
    <html lang="en" className="dark">
      <head>
        <SiteJsonLd />
      </head>
      <body className="min-h-screen bg-[#030303] text-neutral-100 antialiased">
        <a
          href="#main-content"
          className="sr-only focus:not-sr-only focus:fixed focus:left-4 focus:top-4 focus:z-[100] focus:rounded focus:bg-neutral-50 focus:px-4 focus:py-2 focus:text-sm focus:text-black"
        >
          Skip to main content
        </a>
        {children}
      </body>
    </html>
  );
}
