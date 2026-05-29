import type { Metadata } from "next";
import { ReactNode } from "react";
import { SiteHeader } from "../SiteHeader";
import { SiteFooter } from "../SiteFooter";
import { WalletProvider } from "./WalletProvider";
import { NetworkBanner } from "./NetworkBanner";

export const metadata: Metadata = {
  title: "Covenant Stake — lock $CVNT for a share of protocol revenue",
  description:
    "Lock $CVNT for 7, 30, 90, or 180 days and receive a pro-rata share of protocol revenue distributed in SOL. Amounts depend on actual revenue and are not guaranteed.",
  alternates: { canonical: "/stake" },
  openGraph: {
    type: "website",
    url: "https://opencovenant.org/stake",
    title: "Covenant Stake — lock $CVNT for a share of protocol revenue",
    description:
      "Lock $CVNT for 7, 30, 90, or 180 days and receive a pro-rata share of protocol revenue distributed in SOL.",
  },
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
