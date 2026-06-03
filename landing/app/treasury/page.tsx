import type { Metadata } from "next";
import { SiteHeader } from "../SiteHeader";
import { SiteFooter } from "../SiteFooter";
import { WalletProvider } from "../stake/WalletProvider";
import { NetworkBanner } from "../stake/NetworkBanner";
import { TreasuryClient } from "./TreasuryClient";

export const metadata: Metadata = {
  title: "Treasury Dashboard: Covenant Stake On-Chain Metrics",
  description:
    "Public read-only dashboard of the Covenant staking program's on-chain state: cumulative SOL distributions, protocol holdings, and active staker count.",
  alternates: { canonical: "/treasury" },
  openGraph: {
    type: "website",
    url: "https://opencovenant.org/treasury",
    title: "Treasury Dashboard: Covenant Stake On-Chain Metrics",
    description:
      "Public read-only dashboard of the Covenant staking program's on-chain state.",
    images: [{ url: "/opengraph-image.jpg", width: 1200, height: 630 }],
  },
  twitter: {
    card: "summary_large_image",
    site: "@OpenCovenant",
    creator: "@OpenCovenant",
    title: "Treasury Dashboard: Covenant Stake On-Chain Metrics",
    description:
      "Public read-only dashboard of the Covenant staking program's on-chain state.",
    images: "/twitter-image.jpg",
  },
};

export default function TreasuryPage() {
  return (
    <WalletProvider>
      <main id="main-content" className="relative min-h-[100dvh] bg-[#030303] pb-32">
        <SiteHeader />
        <NetworkBanner />
        <div className="page-container">
          <TreasuryClient />
        </div>
        <SiteFooter className="absolute inset-x-0 bottom-6 z-20" />
      </main>
    </WalletProvider>
  );
}
