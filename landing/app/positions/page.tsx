import type { Metadata } from "next";
import { SiteHeader } from "../SiteHeader";
import { SiteFooter } from "../SiteFooter";
import { WalletProvider } from "../stake/WalletProvider";
import { NetworkBanner } from "../stake/NetworkBanner";
import { PositionsClient } from "./PositionsClient";

export const metadata: Metadata = {
  title: "Positions — Covenant Stake",
  description:
    "Manage your locked $CVNT positions, claim accrued SOL, and withdraw principal once the lock period elapses.",
  alternates: { canonical: "/positions" },
  robots: { index: false, follow: false },
  openGraph: {
    type: "website",
    url: "https://opencovenant.org/positions",
    title: "Positions — Covenant Stake",
    description:
      "Manage your locked $CVNT positions, claim accrued SOL, and withdraw principal once the lock period elapses.",
    images: [{ url: "/opengraph-image.jpg", width: 1200, height: 630 }],
  },
  twitter: {
    card: "summary_large_image",
    site: "@OpenCovenant",
    creator: "@OpenCovenant",
    title: "Positions — Covenant Stake",
    description:
      "Manage your locked $CVNT positions, claim accrued SOL, and withdraw principal once the lock period elapses.",
    images: "/twitter-image.jpg",
  },
};

export default function PositionsPage() {
  return (
    <WalletProvider>
      <main id="main-content" className="relative min-h-[100dvh] bg-[#030303] pb-32">
        <SiteHeader />
        <NetworkBanner />
        <div className="page-container">
          <PositionsClient />
        </div>
        <SiteFooter className="absolute inset-x-0 bottom-6 z-20" />
      </main>
    </WalletProvider>
  );
}
