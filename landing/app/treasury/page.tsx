import { SiteHeader } from "../SiteHeader";
import { SiteFooter } from "../SiteFooter";
import { WalletProvider } from "../stake/WalletProvider";
import { NetworkBanner } from "../stake/NetworkBanner";
import { TreasuryClient } from "./TreasuryClient";

export const metadata = {
  title: "Treasury — Covenant Stake",
  description: "Public read-only dashboard of cumulative SOL distributed to stakers and locked $CVNT in the buyback vault.",
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
