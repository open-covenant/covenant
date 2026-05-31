import type { Metadata } from "next";
import { ReactNode } from "react";
import { SiteHeader } from "../SiteHeader";
import { SiteFooter } from "../SiteFooter";
import { WalletProvider } from "./WalletProvider";
import { NetworkBanner } from "./NetworkBanner";

export const metadata: Metadata = {
  title: "Covenant Stake — lock $CVNT for a share of protocol revenue",
  description:
    "Lock $CVNT for 7–180 days and earn a pro-rata share of protocol revenue in SOL. Distribution amounts reflect actual protocol revenue and are not guaranteed.",
  alternates: { canonical: "/stake" },
  openGraph: {
    type: "website",
    url: "https://opencovenant.org/stake",
    title: "Covenant Stake — lock $CVNT for a share of protocol revenue",
    description:
      "Lock $CVNT for 7–180 days and earn a pro-rata share of protocol revenue distributed in SOL.",
    images: [{ url: "/opengraph-image.jpg", width: 1200, height: 630 }],
  },
  twitter: {
    card: "summary_large_image",
    site: "@OpenCovenant",
    creator: "@OpenCovenant",
    title: "Covenant Stake — lock $CVNT for a share of protocol revenue",
    description:
      "Lock $CVNT for 7–180 days and earn a pro-rata share of protocol revenue distributed in SOL.",
    images: "/twitter-image.jpg",
  },
};

export default function StakeLayout({ children }: { children: ReactNode }) {
  return (
    <WalletProvider>
      <main id="main-content" className="relative min-h-[100dvh] bg-[#030303] pb-32">
        <SiteHeader />
        <NetworkBanner />
        <div className="page-container">{children}</div>
        <SiteFooter className="absolute inset-x-0 bottom-6 z-20" />
      </main>
    </WalletProvider>
  );
}
