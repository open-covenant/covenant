import type { Metadata } from "next";
import { SiteHeader } from "../SiteHeader";
import { SiteFooter } from "../SiteFooter";
import { WalletProvider } from "../stake/WalletProvider";
import { NetworkBanner } from "../stake/NetworkBanner";
import { TreasuryClient } from "./TreasuryClient";

export const metadata: Metadata = {
  title: "Treasury — Covenant Stake",
  description:
    "Public read-only dashboard of the Covenant staking program's on-chain state. Cumulative SOL distributed to stakers, protocol-held CVNT, and active position count.",
  alternates: { canonical: "/treasury" },
  openGraph: {
    type: "website",
    url: "https://opencovenant.org/treasury",
    title: "Treasury — Covenant Stake",
    description:
      "Public read-only dashboard of the Covenant staking program's on-chain state.",
  },
};

export default function TreasuryPage() {
  return (
    <WalletProvider>
      <main className="relative min-h-[100dvh] bg-[#030303] pb-32">
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
