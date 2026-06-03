import type { Metadata } from "next";
import Link from "next/link";
import { SiteFooter } from "../SiteFooter";
import { SiteHeader } from "../SiteHeader";
import { ContactForm } from "./ContactForm";

export const metadata: Metadata = {
  title: "Contact: Covenant Partnerships & Security",
  description:
    "Get in touch with the Covenant team about partnerships, integrations, research collaboration, and security disclosures.",
  alternates: { canonical: "/contact" },
  openGraph: {
    type: "website",
    url: "https://opencovenant.org/contact",
    title: "Contact: Covenant Partnerships & Security",
    description:
      "Get in touch with the Covenant team about partnerships, integrations, research collaboration, and security disclosures.",
    images: [{ url: "/opengraph-image.jpg", width: 1200, height: 630 }],
  },
  twitter: {
    card: "summary_large_image",
    site: "@OpenCovenant",
    creator: "@OpenCovenant",
    title: "Contact: Covenant Partnerships & Security",
    description:
      "Get in touch with the Covenant team about partnerships, integrations, research collaboration, and security disclosures.",
    images: "/twitter-image.jpg",
  },
};

export default function ContactPage() {
  return (
    <main id="main-content" className="relative min-h-[100dvh] bg-[#030303] pb-32">
      <SiteHeader />

      <div className="page-container">
        <div className="mb-10 flex flex-col items-start gap-4 border-b border-neutral-900 pb-6 sm:flex-row sm:items-end sm:justify-between sm:gap-6">
          <div>
            <div className="text-[10px] uppercase tracking-[0.3em] text-neutral-500">Covenant</div>
            <h1 className="mt-3 text-3xl font-extralight tracking-tight text-neutral-50 sm:text-4xl">
              Contact
            </h1>
            <p className="mt-3 max-w-xl text-sm leading-relaxed text-neutral-400">
              Partnership inquiries, integrations, research collaboration, and
              responsible disclosure of security issues. All routed to the
              team.
            </p>
          </div>
        </div>

        <div className="grid grid-cols-1 gap-6 lg:grid-cols-[1.1fr_0.9fr]">
          <Panel>
            <PanelEyebrow>Send a message</PanelEyebrow>
            <p className="mt-4 max-w-md text-[13px] leading-relaxed text-neutral-400">
              Fill out the form below and we will respond to the address you
              provide. Typical response time is within two business days.
            </p>
            <div className="mt-8">
              <ContactForm />
            </div>
          </Panel>

          <div className="flex flex-col gap-6">
            <Panel>
              <PanelEyebrow>Direct channels</PanelEyebrow>
              <div className="mt-6 space-y-5">
                <ChannelRow
                  label="Security disclosure"
                  href="mailto:security@opencovenant.org"
                  value="security@opencovenant.org"
                  detail="Vulnerabilities and incidents. Encrypted disclosure available on request."
                />
                <ChannelRow
                  label="Partnerships"
                  href="mailto:partnerships@opencovenant.org"
                  value="partnerships@opencovenant.org"
                  detail="Integrations, distribution, and ecosystem programs."
                />
                <ChannelRow
                  label="GitHub"
                  href="https://github.com/open-covenant/covenant"
                  value="open-covenant/covenant"
                  detail="Issues, pull requests, and the public roadmap."
                />
              </div>
            </Panel>

            <Panel>
              <PanelEyebrow>Resources</PanelEyebrow>
              <div className="mt-6 space-y-3 text-[13px]">
                <ResourceLink href="https://docs.opencovenant.org">
                  Documentation
                </ResourceLink>
                <ResourceLink href="https://doi.org/10.5281/zenodo.20134416">
                  Research paper
                </ResourceLink>
                <ResourceLink href="/roadmap" internal>
                  Public roadmap
                </ResourceLink>
                <ResourceLink href="/stake" internal>
                  Staking program
                </ResourceLink>
              </div>
            </Panel>

            <Panel>
              <PanelEyebrow>About inquiries</PanelEyebrow>
              <ul className="mt-6 space-y-3 text-[12px] leading-relaxed text-neutral-500">
                <li>Press, integration, and ecosystem inquiries are read and triaged daily.</li>
                <li>Security issues take priority. Please mark the message subject accordingly.</li>
                <li>We do not provide individual investment, legal, or tax advice.</li>
              </ul>
            </Panel>
          </div>
        </div>
      </div>

      <SiteFooter className="pb-8" />
    </main>
  );
}

function Panel({ children }: { children: React.ReactNode }) {
  return (
    <section className="rounded-md border border-neutral-900/80 p-6 sm:p-8">
      {children}
    </section>
  );
}

function PanelEyebrow({ children }: { children: React.ReactNode }) {
  return (
    <div className="text-[10px] uppercase tracking-[0.28em] text-neutral-500">{children}</div>
  );
}

function ChannelRow({
  label,
  href,
  value,
  detail,
}: {
  label: string;
  href: string;
  value: string;
  detail: string;
}) {
  return (
    <div className="border-b border-neutral-900 pb-5 last:border-0 last:pb-0">
      <div className="text-[10px] uppercase tracking-[0.22em] text-neutral-500">{label}</div>
      <a
        href={href}
        target={href.startsWith("http") ? "_blank" : undefined}
        rel={href.startsWith("http") ? "noopener noreferrer" : undefined}
        className="mt-2 block font-mono text-sm text-neutral-100 transition-colors hover:text-white"
      >
        {value}
      </a>
      <p className="mt-2 text-[11px] leading-relaxed text-neutral-500">{detail}</p>
    </div>
  );
}

function ResourceLink({
  href,
  internal,
  children,
}: {
  href: string;
  internal?: boolean;
  children: React.ReactNode;
}) {
  const className =
    "flex items-center justify-between border-b border-neutral-900 pb-3 text-neutral-300 transition-colors hover:text-neutral-50 last:border-0 last:pb-0";
  const inner = (
    <>
      <span>{children}</span>
      <span aria-hidden="true" className="text-neutral-600">→</span>
    </>
  );
  if (internal) {
    return (
      <Link href={href} className={className}>
        {inner}
      </Link>
    );
  }
  return (
    <a href={href} target="_blank" rel="noopener noreferrer" className={className}>
      {inner}
    </a>
  );
}
