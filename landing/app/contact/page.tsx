import type { Metadata } from "next";
import { SiteFooter } from "../SiteFooter";
import { SiteHeader } from "../SiteHeader";
import { ContactForm } from "./ContactForm";

export const metadata: Metadata = {
  title: "Contact — Covenant",
  description:
    "Get in touch with the Covenant team — questions, integrations, research collaboration, and security disclosures.",
  alternates: { canonical: "/contact" },
  openGraph: {
    type: "website",
    url: "https://opencovenant.org/contact",
    title: "Contact — Covenant",
    description:
      "Get in touch with the Covenant team — questions, integrations, research collaboration, and security disclosures.",
  },
};

export default function ContactPage() {
  return (
    <main className="relative min-h-screen overflow-x-hidden bg-[#030303]">
      <SiteHeader />

      <div className="page-container">
        <h1 className="mb-16 text-[11px] uppercase tracking-[0.4em] text-neutral-400 sm:mb-20">
          Contact
        </h1>

        <div className="grid gap-12 sm:grid-cols-2 sm:gap-16">
          <div
            aria-hidden="true"
            className="order-last min-h-[320px] w-full self-stretch sm:order-first sm:min-h-0"
            style={{
              backgroundImage: "url(/contact.jpg)",
              backgroundSize: "auto",
              backgroundRepeat: "no-repeat",
              backgroundPosition: "center",
              maskImage:
                "radial-gradient(115% 115% at 50% 50%, #000 62%, transparent 100%)",
              WebkitMaskImage:
                "radial-gradient(115% 115% at 50% 50%, #000 62%, transparent 100%)",
            }}
          />

          <div>
            <p className="mb-10 max-w-md text-[13px] leading-relaxed text-neutral-400 sm:text-[14px]">
              Questions, integrations, research collaboration, or a security
              disclosure — send a message and we&apos;ll get back to you.
            </p>
            <ContactForm />
          </div>
        </div>
      </div>

      <SiteFooter className="pb-8" />
    </main>
  );
}
