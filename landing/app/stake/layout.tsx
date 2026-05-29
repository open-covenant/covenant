import { ReactNode } from "react";
import { SiteHeader } from "../SiteHeader";
import { SiteFooter } from "../SiteFooter";
import { WalletProvider } from "./WalletProvider";
import { NetworkBanner } from "./NetworkBanner";

export const metadata = {
  title: "Covenant Stake — lock $CVNT, earn SOL",
  description:
    "Lock $CVNT for 30, 90, 180, or 365 days and earn pro-rata SOL from pump.fun creator-fee revenue. Real yield, no inflation.",
};

export default function StakeLayout({ children }: { children: ReactNode }) {
  return (
    <WalletProvider>
      <main className="relative min-h-[100dvh] bg-[#030303] pb-32">
        <SiteHeader />
        <NetworkBanner />
        <div className="page-container">{children}</div>
        <SiteFooter className="absolute inset-x-0 bottom-6 z-20" />
      </main>
    </WalletProvider>
  );
}
