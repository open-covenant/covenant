import type { Metadata } from "next";
import Link from "next/link";
import { SiteFooter } from "../SiteFooter";
import { SiteHeader } from "../SiteHeader";

export const metadata: Metadata = {
  title: "Privacy Policy: Covenant",
  description:
    "Privacy policy for opencovenant.org, docs.opencovenant.org, and sandbox.opencovenant.org: what we collect, how we use it, and the third parties involved.",
  alternates: { canonical: "/privacy" },
  openGraph: {
    type: "website",
    url: "https://opencovenant.org/privacy",
    title: "Privacy Policy: Covenant",
    description:
      "What Covenant's public sites collect, why, and the third parties involved.",
    images: [{ url: "/opengraph-image.jpg", width: 1200, height: 630 }],
  },
  twitter: {
    card: "summary_large_image",
    site: "@OpenCovenant",
    creator: "@OpenCovenant",
    title: "Privacy Policy: Covenant",
    description:
      "What Covenant's public sites collect, why, and the third parties involved.",
    images: "/twitter-image.jpg",
  },
};

const LAST_UPDATED = "2026-05-29";

type Section = {
  id: string;
  label: string;
  title: string;
  body: React.ReactNode;
};

const headingEyebrow =
  "font-mono text-[11px] uppercase tracking-[0.3em] text-neutral-400";
const headingTitle =
  "text-[13px] uppercase tracking-[0.25em] text-neutral-100 sm:text-[14px]";
const paragraph =
  "text-[13px] leading-relaxed text-neutral-300 sm:text-[14px]";
const subtle = "text-neutral-400";
const linkClass =
  "underline decoration-neutral-700 underline-offset-4 transition-colors hover:decoration-neutral-300 hover:text-neutral-50";

const SECTIONS: Section[] = [
  {
    id: "scope",
    label: "01",
    title: "Scope",
    body: (
      <p className={paragraph}>
        This policy covers <strong className="text-neutral-100">opencovenant.org</strong> (this
        site), <strong className="text-neutral-100">docs.opencovenant.org</strong> (the
        documentation), and{" "}
        <strong className="text-neutral-100">sandbox.opencovenant.org</strong> (the public
        sandbox). The marketing and docs sites are static informational pages. The sandbox runs
        anonymous coding tasks inside ephemeral, single-use microVMs that are torn down when the
        run finishes. None of these sites ask you to sign up.
      </p>
    ),
  },
  {
    id: "collect",
    label: "02",
    title: "What we collect",
    body: (
      <div className="space-y-5">
        <div>
          <p className={`${paragraph} mb-2`}>
            <span className={subtle}>Contact form (opencovenant.org).</span> Your name, email
            address, and the message you write. We receive them in our inbox so we can reply.
          </p>
        </div>
        <div>
          <p className={`${paragraph} mb-2`}>
            <span className={subtle}>Sandbox (sandbox.opencovenant.org).</span>
          </p>
          <ul className="space-y-2">
            <li className="flex gap-3 text-[13px] leading-relaxed text-neutral-300 sm:text-[14px]">
              <span aria-hidden="true" className="select-none text-neutral-600">
                ·
              </span>
              <span>
                The text of the build request you submit (the &ldquo;intent&rdquo;).
              </span>
            </li>
            <li className="flex gap-3 text-[13px] leading-relaxed text-neutral-300 sm:text-[14px]">
              <span aria-hidden="true" className="select-none text-neutral-600">
                ·
              </span>
              <span>
                Output of the run, including files the agent wrote in the sandbox, terminal
                output, and the response, held only long enough to render it back in your browser.
              </span>
            </li>
            <li className="flex gap-3 text-[13px] leading-relaxed text-neutral-300 sm:text-[14px]">
              <span aria-hidden="true" className="select-none text-neutral-600">
                ·
              </span>
              <span>
                Your IP address (or the right-most value of <code>X-Forwarded-For</code> from a
                proxy we trust), held in memory to enforce a per-IP rate limit so a single
                client can&apos;t drain the daily budget. It is not written to disk.
              </span>
            </li>
            <li className="flex gap-3 text-[13px] leading-relaxed text-neutral-300 sm:text-[14px]">
              <span aria-hidden="true" className="select-none text-neutral-600">
                ·
              </span>
              <span>
                Entries in the append-only audit chain the daemon writes for each run, covering
                agent name, tool invocations, run durations, and the paths and byte counts of
                files the sandbox wrote. The intent text is included.
              </span>
            </li>
          </ul>
        </div>
        <div>
          <p className={paragraph}>
            <span className={subtle}>Bot detection.</span> The sandbox uses{" "}
            <strong className="text-neutral-100">Cloudflare Turnstile</strong> in
            interaction-only mode to refuse automated traffic. Cloudflare receives the device
            and network signals it needs to score the request. We reference{" "}
            <strong className="text-neutral-100">Cloudflare&apos;s Turnstile Privacy Addendum</strong>{" "}
            (published as part of Cloudflare&apos;s privacy policy at{" "}
            <a
              href="https://www.cloudflare.com/privacypolicy/"
              target="_blank"
              rel="noopener noreferrer"
              className={linkClass}
            >
              cloudflare.com/privacypolicy
            </a>
            ) for the specific signals Turnstile processes and how Cloudflare uses them.
            Loading the widget sets short-lived cookies on Cloudflare&apos;s challenge
            subdomain.
          </p>
        </div>
        <div>
          <p className={paragraph}>
            <span className={subtle}>Cookies, analytics, tracking.</span> None on
            opencovenant.org or docs.opencovenant.org. None on sandbox.opencovenant.org other
            than the Cloudflare cookies described above.
          </p>
        </div>
      </div>
    ),
  },
  {
    id: "use",
    label: "03",
    title: "How we use it",
    body: (
      <ul className="space-y-2">
        {[
          "Reply to your contact message.",
          "Run your sandbox task and stream the result back to your browser.",
          "Refuse bot traffic and rate-limit clients that try to exhaust the daily budget.",
          "Keep a tamper-evident record of what each run did. The audit chain is one of Covenant's core security guarantees.",
        ].map((line) => (
          <li
            key={line}
            className="flex gap-3 text-[13px] leading-relaxed text-neutral-300 sm:text-[14px]"
          >
            <span aria-hidden="true" className="select-none text-neutral-600">
              ·
            </span>
            <span>{line}</span>
          </li>
        ))}
        <li className="flex gap-3 pt-3 text-[13px] leading-relaxed text-neutral-400 sm:text-[14px]">
          <span aria-hidden="true" className="select-none text-neutral-600">
            ·
          </span>
          <span>
            We don&apos;t sell what we collect. We don&apos;t share it with advertising
            networks. There are no advertising networks on these sites.
          </span>
        </li>
      </ul>
    ),
  },
  {
    id: "third-parties",
    label: "04",
    title: "Third parties",
    body: (
      <ul className="space-y-3">
        <li className="text-[13px] leading-relaxed text-neutral-300 sm:text-[14px]">
          <strong className="text-neutral-100">Cloudflare</strong>. Bot detection on the
          sandbox via Turnstile, in interaction-only mode. See{" "}
          <a
            href="https://www.cloudflare.com/privacypolicy/"
            target="_blank"
            rel="noopener noreferrer"
            className={linkClass}
          >
            cloudflare.com/privacypolicy
          </a>{" "}
          and the Turnstile Privacy Addendum referenced there.
        </li>
        <li className="text-[13px] leading-relaxed text-neutral-300 sm:text-[14px]">
          <strong className="text-neutral-100">Resend</strong>. Delivers the contact form
          emails to our inbox. See{" "}
          <a
            href="https://resend.com/legal/privacy-policy"
            target="_blank"
            rel="noopener noreferrer"
            className={linkClass}
          >
            resend.com/legal/privacy-policy
          </a>
          .
        </li>
        <li className="text-[13px] leading-relaxed text-neutral-300 sm:text-[14px]">
          <strong className="text-neutral-100">Anthropic</strong>. Runs the underlying coding
          model that powers sandbox runs. Your intent text and any context the model needs to
          do its job are sent to Anthropic. See{" "}
          <a
            href="https://www.anthropic.com/legal/privacy"
            target="_blank"
            rel="noopener noreferrer"
            className={linkClass}
          >
            anthropic.com/legal/privacy
          </a>
          .
        </li>
        <li className="text-[13px] leading-relaxed text-neutral-300 sm:text-[14px]">
          <strong className="text-neutral-100">E2B</strong>. Provisions the ephemeral microVM
          your sandbox run executes in. See{" "}
          <a
            href="https://e2b.dev/privacy"
            target="_blank"
            rel="noopener noreferrer"
            className={linkClass}
          >
            e2b.dev/privacy
          </a>
          .
        </li>
      </ul>
    ),
  },
  {
    id: "retention",
    label: "05",
    title: "Retention",
    body: (
      <ul className="space-y-2">
        <li className="flex gap-3 text-[13px] leading-relaxed text-neutral-300 sm:text-[14px]">
          <span aria-hidden="true" className="select-none text-neutral-600">
            ·
          </span>
          <span>
            <span className={subtle}>Contact emails.</span> Until we delete them from our
            inbox. No automated deletion schedule.
          </span>
        </li>
        <li className="flex gap-3 text-[13px] leading-relaxed text-neutral-300 sm:text-[14px]">
          <span aria-hidden="true" className="select-none text-neutral-600">
            ·
          </span>
          <span>
            <span className={subtle}>Sandbox run output and audit chain.</span> Kept as long
            as the daemon&apos;s store keeps it. The chain is designed for tamper-evident
            retention, not aggressive deletion.
          </span>
        </li>
        <li className="flex gap-3 text-[13px] leading-relaxed text-neutral-300 sm:text-[14px]">
          <span aria-hidden="true" className="select-none text-neutral-600">
            ·
          </span>
          <span>
            <span className={subtle}>Rate-limit state.</span> In memory only. Cleared when the
            gateway restarts.
          </span>
        </li>
      </ul>
    ),
  },
  {
    id: "choices",
    label: "06",
    title: "Your choices",
    body: (
      <ul className="space-y-2">
        <li className="flex gap-3 text-[13px] leading-relaxed text-neutral-300 sm:text-[14px]">
          <span aria-hidden="true" className="select-none text-neutral-600">
            ·
          </span>
          <span>
            The sandbox needs no account. If you don&apos;t want a request recorded,
            don&apos;t submit it.
          </span>
        </li>
        <li className="flex gap-3 text-[13px] leading-relaxed text-neutral-300 sm:text-[14px]">
          <span aria-hidden="true" className="select-none text-neutral-600">
            ·
          </span>
          <span>
            To ask us to delete a contact message or a specific sandbox run, write us via{" "}
            <Link href="/contact" className={linkClass}>
              contact
            </Link>
            .
          </span>
        </li>
        <li className="flex gap-3 text-[13px] leading-relaxed text-neutral-300 sm:text-[14px]">
          <span aria-hidden="true" className="select-none text-neutral-600">
            ·
          </span>
          <span>
            Blocking Cloudflare cookies will block the Turnstile check, which will prevent
            sandbox submissions.
          </span>
        </li>
      </ul>
    ),
  },
  {
    id: "security",
    label: "07",
    title: "Security disclosures",
    body: (
      <p className={paragraph}>
        Report a security issue via{" "}
        <Link href="/contact" className={linkClass}>
          contact
        </Link>
        . We aim to acknowledge within a few business days.
      </p>
    ),
  },
  {
    id: "changes",
    label: "08",
    title: "Changes",
    body: (
      <p className={paragraph}>
        When we change this policy, the &ldquo;last updated&rdquo; date at the top moves and
        the change is committed to the public repository at{" "}
        <a
          href="https://github.com/open-covenant/covenant"
          target="_blank"
          rel="noopener noreferrer"
          className={linkClass}
        >
          github.com/open-covenant/covenant
        </a>
        .
      </p>
    ),
  },
  {
    id: "contact",
    label: "09",
    title: "Contact",
    body: (
      <p className={paragraph}>
        Questions about this policy:{" "}
        <Link href="/contact" className={linkClass}>
          opencovenant.org/contact
        </Link>
        .
      </p>
    ),
  },
];

export default function PrivacyPage() {
  return (
    <main id="main-content" className="relative min-h-screen overflow-x-hidden bg-[#030303]">
      <SiteHeader />

      <div className="page-container">
        <div className="mb-16 sm:mb-24">
          <h1 className="mb-3 text-[11px] uppercase tracking-[0.4em] text-neutral-400">
            Privacy
          </h1>
          <p className="text-[11px] uppercase tracking-[0.3em] text-neutral-400">
            Last updated · {LAST_UPDATED}
          </p>
        </div>

        <ol className="relative">
          {SECTIONS.map((s, i) => {
            const isLast = i === SECTIONS.length - 1;
            return (
              <li
                key={s.id}
                id={s.id}
                className={`relative border-l border-neutral-800/80 pl-8 sm:pl-12 ${isLast ? "" : "pb-14 sm:pb-16"}`}
              >
                <span
                  aria-hidden="true"
                  className="absolute -left-[5px] top-[7px] h-[9px] w-[9px] rounded-full border border-neutral-700 bg-[#030303]"
                />
                <div className="mb-4 flex flex-wrap items-baseline gap-x-4 gap-y-1">
                  <span className={headingEyebrow}>{s.label}</span>
                  <h2 className={headingTitle}>{s.title}</h2>
                </div>
                {s.body}
              </li>
            );
          })}
        </ol>
      </div>

      <SiteFooter className="pb-8" />
    </main>
  );
}
