import type { Metadata } from "next";
import Script from "next/script";
import { CopyAddress } from "../CopyAddress";
import { SiteFooter } from "../SiteFooter";
import { SiteHeader } from "../SiteHeader";

export const metadata: Metadata = {
  title: "Buy $CVNT",
  description:
    "Buy $CVNT with a card or crypto, no wallet setup required. The token that powers Covenant, the open operating layer for agentic software.",
  alternates: { canonical: "/token" },
  openGraph: {
    type: "website",
    url: "https://opencovenant.org/token",
    title: "Buy $CVNT",
    description:
      "Card or crypto, no seed phrase. The token that powers Covenant, the open operating layer for agentic software.",
    images: [{ url: "/opengraph-image.jpg", width: 1200, height: 630 }],
  },
  twitter: {
    card: "summary_large_image",
    site: "@OpenCovenant",
    creator: "@OpenCovenant",
    title: "Buy $CVNT",
    description: "Card or crypto, no seed phrase. The token that powers Covenant.",
    images: "/twitter-image.jpg",
  },
};

const eyebrow = "font-mono text-[11px] uppercase tracking-[0.3em] text-neutral-500";
const headingTitle = "text-[13px] uppercase tracking-[0.25em] text-neutral-100 sm:text-[14px]";
const paragraph = "text-[13px] leading-relaxed text-neutral-300 sm:text-[14px]";
const linkClass =
  "underline decoration-neutral-700 underline-offset-4 transition-colors hover:decoration-neutral-300 hover:text-neutral-50";

const CTA = [
  { label: "Stake $CVNT", href: "/stake" },
  { label: "Treasury", href: "/treasury" },
  { label: "About the token", href: "/about#real" },
  { label: "Documentation", href: "https://docs.opencovenant.org" },
];

export default function TokenPage() {
  return (
    <main id="main-content" className="relative min-h-screen overflow-x-hidden bg-[#030303]">
      <SiteHeader />

      <div className="page-container">
        <h1 className="mb-10 text-[11px] uppercase tracking-[0.4em] text-neutral-400 sm:mb-14">
          Token
        </h1>

        <h2 className="max-w-3xl text-balance text-[2.2rem] font-extralight uppercase leading-[1.1] tracking-[2px] text-white sm:text-[2.6rem]">
          Buy $CVNT
        </h2>

        <p className="mt-6 max-w-2xl text-pretty text-lg font-light leading-relaxed text-neutral-300 sm:text-xl">
          $CVNT is Covenant&apos;s token on Solana, with a fixed supply and a renounced mint
          authority. Buy it below with a card or with crypto. No seed phrase, no bridging, no
          juggling apps. A wallet is created for you the first time you need one.
        </p>

        {/* Qwerti embed: card-or-crypto buy for $CVNT, white-labeled into the page.
            Campaign id binds the buy to Covenant's payout config on Qwerti's side. */}
        <section className="mt-12 sm:mt-16" aria-label="Buy $CVNT">
          <div className="mb-4 flex flex-wrap items-baseline gap-x-4 gap-y-1">
            <span className={eyebrow}>$CVNT</span>
            <h2 className={headingTitle}>Card or crypto</h2>
          </div>
          <div
            id="qwerti-widget"
            className="min-h-[420px] w-full max-w-md rounded-lg border border-neutral-800/80 bg-[#070707] p-1"
          >
            <noscript>
              <p className={`p-6 ${paragraph}`}>
                The buy widget needs JavaScript. You can also buy $CVNT directly on{" "}
                <a
                  href="https://pump.fun/coin/2mNVZ6aEjrGwiUVCfz7XGWpiXuWzgBDoznwE579upump"
                  target="_blank"
                  rel="noopener noreferrer"
                  className={linkClass}
                >
                  pump.fun
                </a>
                .
              </p>
            </noscript>
          </div>
          <Script
            src="https://widget.qwerti.ai/widget/v1/buy.js"
            strategy="afterInteractive"
            data-widget="qwerti-widget"
            data-campaign="$cvnt-792703809-77184"
            data-auto-open="true"
          />
        </section>

        <div className="mt-16 border-t border-neutral-800/80 pt-8 sm:mt-24">
          <div className="mb-4 flex flex-wrap items-baseline gap-x-4 gap-y-1">
            <span className={eyebrow}>$CVNT</span>
            <h2 className={headingTitle}>How the token works</h2>
          </div>
          <p className={`max-w-2xl ${paragraph}`}>
            As Covenant gets used, the network earns revenue, and that revenue is shared
            automatically: part goes to people who stake $CVNT, part is used to buy the token on the
            open market and lock it away, and the rest funds the treasury. Stake to earn a share,
            paid in SOL, with longer locks earning more. You can stake on the{" "}
            <a href="/stake" className={linkClass}>
              stake page
            </a>
            .
          </p>
          <div className="mt-5">
            <span className={`${eyebrow} mb-2 block`}>Contract address</span>
            <CopyAddress
              address="2mNVZ6aEjrGwiUVCfz7XGWpiXuWzgBDoznwE579upump"
              label="Copy $CVNT contract address"
            />
          </div>
        </div>

        <div className="mt-16 flex flex-wrap gap-x-6 gap-y-3 border-t border-neutral-800/80 pt-8 sm:mt-24">
          {CTA.map((c) => {
            const external = c.href.startsWith("http");
            return (
              <a
                key={c.href}
                href={c.href}
                {...(external ? { target: "_blank", rel: "noopener noreferrer" } : {})}
                className="text-[12px] uppercase tracking-[0.2em] text-neutral-400 transition-colors hover:text-neutral-50"
              >
                {c.label} →
              </a>
            );
          })}
        </div>
      </div>

      <SiteFooter className="pb-8" />
    </main>
  );
}
