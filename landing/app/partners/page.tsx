import type { Metadata } from "next";
import Image from "next/image";
import { SiteFooter } from "../SiteFooter";
import { SiteHeader } from "../SiteHeader";
import {
  logoFor,
  PARTNERS,
  PROTOCOLS,
  type Integration,
} from "../_partners";

export const metadata: Metadata = {
  title: "Partners & integrations",
  description:
    "The protocols, chains, and products that plug into Covenant: open standards Covenant speaks (Solana, x402, MCP, A2A) and partner integrations across the agent ecosystem.",
  alternates: { canonical: "/partners" },
  openGraph: {
    type: "website",
    url: "https://opencovenant.org/partners",
    title: "Covenant partners & integrations",
    description:
      "Open standards Covenant speaks, and the products that integrate with the operating layer for agentic software.",
    images: [{ url: "/opengraph-image.jpg", width: 1200, height: 630 }],
  },
  twitter: {
    card: "summary_large_image",
    site: "@OpenCovenant",
    creator: "@OpenCovenant",
    title: "Covenant partners & integrations",
    description:
      "Open standards Covenant speaks, and the products that integrate with the operating layer for agentic software.",
    images: "/twitter-image.jpg",
  },
};

const eyebrow = "font-mono text-[11px] uppercase tracking-[0.3em] text-neutral-400";
const headingTitle = "text-[13px] uppercase tracking-[0.25em] text-neutral-100 sm:text-[14px]";
const paragraph = "text-[13px] leading-relaxed text-neutral-300 sm:text-[14px]";

const CTA = [
  { label: "Get in touch", href: "/contact" },
  { label: "Documentation", href: "https://docs.opencovenant.org" },
  { label: "Source", href: "https://github.com/open-covenant/covenant" },
];

function StatusBadge({ status }: { status: Integration["status"] }) {
  const live = status === "live";
  return (
    <span className="inline-flex shrink-0 items-center gap-1.5 font-mono text-[10px] uppercase tracking-[0.2em] text-neutral-400">
      <span
        aria-hidden="true"
        className={`h-1.5 w-1.5 rounded-full ${live ? "bg-emerald-400" : "bg-amber-400/80"}`}
      />
      {live ? "Live" : "In development"}
    </span>
  );
}

/** Avatar (X profile image) beside the wordmark, or just the wordmark if none. */
function Mark({ item }: { item: Integration }) {
  const logo = item.logo ?? logoFor(item.slug);
  return (
    <span className="flex items-center gap-2.5">
      {logo ? (
        <Image
          src={logo}
          alt=""
          width={28}
          height={28}
          className="h-7 w-7 shrink-0 rounded-md object-cover ring-1 ring-neutral-700/50"
        />
      ) : null}
      <span className="text-[15px] font-light tracking-tight text-neutral-100">
        {item.name}
      </span>
    </span>
  );
}

function Card({ item, showStatus }: { item: Integration; showStatus: boolean }) {
  const inner = (
    <>
      <div className="flex items-start justify-between gap-3">
        <Mark item={item} />
        {showStatus ? <StatusBadge status={item.status} /> : null}
      </div>
      <p className={`mt-3 ${paragraph}`}>{item.blurb}</p>
      <div className="mt-auto flex items-end justify-between gap-3 pt-4">
        {item.by ? (
          <span className="font-mono text-[10px] uppercase tracking-[0.2em] text-neutral-500">
            via {item.by}
          </span>
        ) : (
          <span />
        )}
        {item.href ? (
          <span
            aria-hidden="true"
            className="font-mono text-[12px] text-neutral-500 transition-colors group-hover:text-neutral-200"
          >
            ↗
          </span>
        ) : null}
      </div>
    </>
  );

  const className =
    "group flex h-full min-h-[150px] flex-col border border-neutral-800/80 p-5 transition-colors hover:border-neutral-700 hover:bg-neutral-950/40";

  return item.href ? (
    <a
      href={item.href}
      target="_blank"
      rel="noopener noreferrer"
      className={className}
    >
      {inner}
    </a>
  ) : (
    <div className={className}>{inner}</div>
  );
}

export default function PartnersPage() {
  return (
    <main id="main-content" className="relative min-h-screen overflow-x-hidden bg-[#030303]">
      <SiteHeader />

      <div className="page-container">
        <h1 className="mb-10 text-[11px] uppercase tracking-[0.4em] text-neutral-400 sm:mb-14">
          Partners
        </h1>

        <h2 className="max-w-3xl text-balance text-[2.2rem] font-extralight uppercase leading-[1.1] tracking-[2px] text-white sm:text-[2.6rem]">
          Built to plug in
        </h2>

        <p className="mt-6 max-w-2xl text-pretty text-lg font-light leading-relaxed text-neutral-300 sm:text-xl">
          Covenant is open infrastructure, not a walled garden. It speaks open protocols, so any
          compliant client or chain interoperates, and it carries first-class integrations with
          products across the agent ecosystem. Every integration runs under the same signed
          permissions and tamper-evident audit log as the rest of the system.
        </p>

        {/* Protocols & standards */}
        <section className="mt-16 sm:mt-24">
          <div className="mb-6 flex flex-wrap items-baseline gap-x-4 gap-y-1 border-t border-neutral-800/80 pt-8">
            <span className={eyebrow}>01</span>
            <h2 className={headingTitle}>Protocols &amp; standards</h2>
          </div>
          <p className={`mb-8 max-w-2xl ${paragraph}`}>
            The open standards Covenant speaks natively. No bespoke partnership required: any
            compliant tool, agent, or chain interoperates out of the box.
          </p>
          <div className="grid grid-cols-1 gap-3 sm:grid-cols-2 sm:gap-4 lg:grid-cols-4">
            {PROTOCOLS.map((item) => (
              <Card key={item.slug} item={item} showStatus={false} />
            ))}
          </div>
        </section>

        {/* Integrations & partners */}
        <section className="mt-16 sm:mt-24">
          <div className="mb-6 flex flex-wrap items-baseline gap-x-4 gap-y-1 border-t border-neutral-800/80 pt-8">
            <span className={eyebrow}>02</span>
            <h2 className={headingTitle}>Integrations &amp; partners</h2>
          </div>
          <p className={`mb-8 max-w-2xl ${paragraph}`}>
            Products that integrate with Covenant.{" "}
            <span className="text-neutral-200">Live</span> integrations are shipped and running;{" "}
            <span className="text-neutral-200">in development</span> integrations are in active
            build and not yet on the main line.
          </p>
          <div className="grid grid-cols-1 gap-3 sm:grid-cols-2 sm:gap-4 lg:grid-cols-3">
            {PARTNERS.map((item) => (
              <Card key={item.slug} item={item} showStatus />
            ))}
          </div>
        </section>

        {/* CTA */}
        <div className="mt-16 border-t border-neutral-800/80 pt-8 sm:mt-24">
          <p className={`max-w-2xl ${paragraph}`}>
            Building something on Covenant, or want your product to integrate? The protocol surface
            is open and documented — reach out and we&apos;ll help you wire it in.
          </p>
          <div className="mt-6 flex flex-wrap gap-x-6 gap-y-3">
            {CTA.map((c) => {
              const external = c.href.startsWith("http");
              return (
                <a
                  key={c.href}
                  href={c.href}
                  {...(external
                    ? { target: "_blank", rel: "noopener noreferrer" }
                    : {})}
                  className="text-[12px] uppercase tracking-[0.2em] text-neutral-400 transition-colors hover:text-neutral-50"
                >
                  {c.label} →
                </a>
              );
            })}
          </div>
        </div>
      </div>

      <SiteFooter className="pb-8" />
    </main>
  );
}
