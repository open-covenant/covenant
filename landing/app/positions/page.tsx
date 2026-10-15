import type { Metadata } from "next";
import { pageMetadata } from "@/app/_metadata";
import { SiteHeader } from "../SiteHeader";
import { SiteFooter } from "../SiteFooter";
import { WalletProvider } from "../stake/WalletProvider";
import { NetworkBanner } from "../stake/NetworkBanner";
import { PositionsClient } from "./PositionsClient";

export const metadata: Metadata = pageMetadata("/positions", { title: "Staking positions", section: "Covenant Stake" }, {
  title: "Positions: Covenant Stake",
  description:
    "Manage your $CVNT positions, claim accrued SOL, and withdraw principal at any time as the program winds down.",
  alternates: { canonical: "/positions" },
  robots: { index: false, follow: false },
  openGraph: {
    type: "website",
    url: "https://opencovenant.org/positions",
    title: "Positions: Covenant Stake",
    description:
      "Manage your $CVNT positions, claim accrued SOL, and withdraw principal at any time as the program winds down.",
  },
  twitter: {
    card: "summary_large_image",
    site: "@OpenCovenant",
    creator: "@OpenCovenant",
    title: "Positions: Covenant Stake",
    description:
      "Manage your $CVNT positions, claim accrued SOL, and withdraw principal at any time as the program winds down.",
  },
});

export default function PositionsPage() {
  return (
    <WalletProvider>
      <main id="main-content" className="relative min-h-[100dvh] bg-ground pb-32">
        <SiteHeader />
        <NetworkBanner />
        <div className="page-container">
          <PositionsClient />
        </div>
        <SiteFooter variant="minimal" className="absolute inset-x-0 bottom-6 z-20" />
      </main>
    </WalletProvider>
  );
}
