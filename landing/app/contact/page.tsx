import type { Metadata } from "next";
import Image from "next/image";
import Link from "next/link";
import { MobileMenu } from "../MobileMenu";
import { SiteFooter } from "../SiteFooter";
import { GithubIcon, GITHUB_URL, NAV_LINKS, XIcon, X_URL } from "../_brand";
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
      <div
        aria-hidden="true"
        className="pointer-events-none absolute inset-0 z-0"
        style={{
          backgroundImage: "url(/contact.jpg)",
          backgroundSize: "cover",
          backgroundRepeat: "no-repeat",
          backgroundPosition: "left center",
          maskImage:
            "radial-gradient(75% 90% at 22% 50%, #000 50%, transparent 100%)",
          WebkitMaskImage:
            "radial-gradient(75% 90% at 22% 50%, #000 50%, transparent 100%)",
        }}
      />
      <header className="pointer-events-none absolute inset-x-0 top-0 z-20 flex justify-center pt-[max(35px,env(safe-area-inset-top))] sm:pt-[max(59px,env(safe-area-inset-top))]">
        <Link href="/" className="pointer-events-auto" aria-label="Covenant home">
          <Image
            src="/logo.svg"
            alt="covenant"
            width={255}
            height={54}
            priority
            className="h-[42px] w-auto opacity-95 sm:h-[60px]"
          />
        </Link>
      </header>

      <div
        className="absolute left-2 z-20 sm:left-8 sm:top-10"
        style={{ top: "max(0.5rem, env(safe-area-inset-top))" }}
      >
        <div className="sm:hidden">
          <MobileMenu items={NAV_LINKS} />
        </div>
        <nav className="hidden items-center gap-3 sm:flex">
          {NAV_LINKS.map((item) =>
            item.external ? (
              <a
                key={item.href}
                href={item.href}
                target="_blank"
                rel="noopener noreferrer"
                className="px-3 py-3 text-[12px] uppercase tracking-[0.2em] text-neutral-400 transition-colors hover:text-neutral-50"
              >
                {item.label}
              </a>
            ) : (
              <Link
                key={item.href}
                href={item.href}
                className="px-3 py-3 text-[12px] uppercase tracking-[0.2em] text-neutral-400 transition-colors hover:text-neutral-50"
              >
                {item.label}
              </Link>
            ),
          )}
        </nav>
      </div>

      <nav
        className="absolute right-2 z-20 flex items-center gap-1 sm:right-8 sm:top-10 sm:gap-3"
        style={{ top: "max(0.5rem, env(safe-area-inset-top))" }}
      >
        <a
          href={X_URL}
          target="_blank"
          rel="noopener noreferrer"
          aria-label="Covenant on X"
          className="p-3 text-neutral-400 transition-colors hover:text-neutral-50"
        >
          <XIcon className="h-5 w-5" />
        </a>
        <a
          href={GITHUB_URL}
          target="_blank"
          rel="noopener noreferrer"
          aria-label="Covenant on GitHub"
          className="p-3 text-neutral-400 transition-colors hover:text-neutral-50"
        >
          <GithubIcon className="h-5 w-5" />
        </a>
      </nav>

      <div className="relative z-10 mx-auto max-w-2xl px-6 pb-32 pt-[180px] sm:max-w-5xl sm:px-8 sm:pt-[220px]">
        <div className="sm:ml-auto sm:max-w-md">
          <h1 className="mb-16 text-[11px] uppercase tracking-[0.4em] text-neutral-400 sm:mb-20">
            Contact
          </h1>
          <p className="mb-10 text-[13px] leading-relaxed text-neutral-400 sm:text-[14px]">
            Questions, integrations, research collaboration, or a security
            disclosure — send a message and we&apos;ll get back to you.
          </p>
          <ContactForm />
        </div>
      </div>

      <SiteFooter className="pb-8" />
    </main>
  );
}
