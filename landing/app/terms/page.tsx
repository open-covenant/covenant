import type { Metadata } from "next";
import Link from "next/link";
import { SiteFooter } from "../SiteFooter";
import { SiteHeader } from "../SiteHeader";

export const metadata: Metadata = {
  title: "Terms of Service — Covenant",
  description:
    "Terms of service for opencovenant.org, sandbox.opencovenant.org, and the Covenant staking program — acceptable use, alpha-software disclaimers, and operator responsibilities.",
  alternates: { canonical: "/terms" },
  openGraph: {
    type: "website",
    url: "https://opencovenant.org/terms",
    title: "Terms of Service — Covenant",
    description:
      "Acceptable use, alpha-software disclaimers, and operator responsibilities for the Covenant public services.",
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
  "font-mono text-[11px] uppercase tracking-[0.3em] text-neutral-500";
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
      <div className="space-y-5">
        <p className={paragraph}>
          These Terms of Service (the &ldquo;Terms&rdquo;) govern your use of the public services
          operated under the Covenant brand, including{" "}
          <strong className="text-neutral-100">opencovenant.org</strong>,{" "}
          <strong className="text-neutral-100">docs.opencovenant.org</strong>,{" "}
          <strong className="text-neutral-100">sandbox.opencovenant.org</strong>, and the on-chain
          staking program at{" "}
          <span className="font-mono text-neutral-100">CstkpU2q9RngbHh21WVAYeQjbN9UWgcH9pAiQcMaEcED</span>{" "}
          on Solana mainnet (together, the &ldquo;Services&rdquo;).
        </p>
        <p className={paragraph}>
          The Covenant <strong className="text-neutral-100">source code</strong> is published
          under the Apache License 2.0 at{" "}
          <a
            href="https://github.com/open-covenant/covenant"
            target="_blank"
            rel="noopener noreferrer"
            className={linkClass}
          >
            github.com/open-covenant/covenant
          </a>{" "}
          and is governed solely by that license. These Terms apply to the{" "}
          <em>hosted Services</em> only.
        </p>
      </div>
    ),
  },
  {
    id: "acceptance",
    label: "02",
    title: "Acceptance",
    body: (
      <p className={paragraph}>
        By using any of the Services you agree to these Terms and to our{" "}
        <Link href="/privacy" className={linkClass}>
          Privacy Policy
        </Link>
        . If you do not agree, do not use the Services. If you are using the Services on behalf of
        an organization, you represent that you are authorized to bind that organization to these
        Terms.
      </p>
    ),
  },
  {
    id: "alpha",
    label: "03",
    title: "Alpha software",
    body: (
      <p className={paragraph}>
        The Services are in active development and provided as alpha software. Functionality may
        change, regress, or be removed without notice. Bugs, downtime, data loss, and unexpected
        behavior are possible. The Services should not be relied upon for production workloads or
        for purposes that require uninterrupted availability.
      </p>
    ),
  },
  {
    id: "acceptable-use",
    label: "04",
    title: "Acceptable use",
    body: (
      <div className="space-y-3">
        <p className={paragraph}>You agree not to use the Services to:</p>
        <ul className="space-y-2">
          {[
            "develop, distribute, or test malicious code, including malware, ransomware, network attacks, or credential-harvesting tools;",
            "generate or distribute content that is unlawful, infringes third-party rights, or violates applicable export-control or sanctions law;",
            "circumvent or attempt to circumvent the sandbox's resource limits, the bot-check, rate limits, or any other technical control;",
            "automate, scrape, or otherwise overwhelm the Services in a manner not authorized in writing by the operator;",
            "reverse-engineer or extract the system prompts, internal tooling, or third-party API keys used by the sandbox;",
            "use the Services in connection with regulated activity (including unlicensed money transmission or the offer or sale of securities) without your own independent legal counsel and compliance.",
          ].map((point) => (
            <li
              key={point}
              className="flex gap-3 text-[13px] leading-relaxed text-neutral-300 sm:text-[14px]"
            >
              <span aria-hidden="true" className="select-none text-neutral-600">
                ·
              </span>
              <span>{point}</span>
            </li>
          ))}
        </ul>
        <p className={`${paragraph} pt-2`}>
          We may, in our sole discretion, throttle, restrict, or terminate access for any user or
          IP address that we reasonably believe is violating these Terms or impairing the Services
          for other users.
        </p>
      </div>
    ),
  },
  {
    id: "sandbox",
    label: "05",
    title: "Sandbox-specific terms",
    body: (
      <div className="space-y-5">
        <p className={paragraph}>
          <span className={subtle}>Ephemerality.</span> Sandbox runs execute inside single-use
          virtual machines that are destroyed when the run finishes. We do not persist your
          conversation, intermediate files, or generated artifacts beyond what is required to
          render the response to your browser session.
        </p>
        <p className={paragraph}>
          <span className={subtle}>Operator-paid model calls.</span> The sandbox is currently free
          to use and the operator pays the upstream language-model providers. The operator may at
          any time meter usage, introduce credit balances, require bring-your-own-key, or
          discontinue the free tier.
        </p>
        <p className={paragraph}>
          <span className={subtle}>Content responsibility.</span> You are responsible for the
          prompts you submit and for the use you make of the outputs. Outputs are not reviewed for
          accuracy, safety, or licensing compatibility before being returned. You bear all risk
          associated with executing, distributing, or relying on sandbox output.
        </p>
      </div>
    ),
  },
  {
    id: "staking",
    label: "06",
    title: "Staking program",
    body: (
      <div className="space-y-5">
        <p className={paragraph}>
          <span className={subtle}>What it is.</span> The Covenant staking program is an on-chain
          Solana program that lets holders of $CVNT lock tokens for fixed periods (30, 90, 180, or
          365 days) and earn a share of protocol revenue distributed in SOL. All program state is
          public and readable directly from Solana mainnet.
        </p>
        <p className={paragraph}>
          <span className={subtle}>Lockup.</span> Locked principal is non-transferable and cannot
          be withdrawn before the elected lock period ends. There is no early-exit mechanism.
        </p>
        <p className={paragraph}>
          <span className={subtle}>No guarantee of distributions.</span> Distribution amounts
          reflect actual protocol revenue and are not guaranteed. Past distributions are not
          indicative of future distributions, and any displayed rate is a backward-looking
          observation, not a forward promise.
        </p>
        <p className={paragraph}>
          <span className={subtle}>Pause.</span> The protocol may be paused by the operator in
          response to incidents. While paused, new positions and reward claims are blocked.
          Principal withdrawal after lock expiry remains available regardless of pause state.
        </p>
        <p className={paragraph}>
          <span className={subtle}>Not investment advice.</span> Nothing on the Services
          constitutes investment, legal, tax, or accounting advice. The Services do not solicit or
          recommend the purchase, sale, or holding of any digital asset. Cryptocurrencies are
          volatile and you may lose the entire value of your position.
        </p>
      </div>
    ),
  },
  {
    id: "no-warranty",
    label: "07",
    title: "No warranty",
    body: (
      <p className={paragraph}>
        THE SERVICES ARE PROVIDED &ldquo;AS IS&rdquo; AND &ldquo;AS AVAILABLE&rdquo;, WITHOUT
        WARRANTIES OF ANY KIND, EXPRESS OR IMPLIED, INCLUDING WITHOUT LIMITATION ANY IMPLIED
        WARRANTY OF MERCHANTABILITY, FITNESS FOR A PARTICULAR PURPOSE, NON-INFRINGEMENT, OR
        UNINTERRUPTED OR ERROR-FREE OPERATION. NO ADVICE OR INFORMATION OBTAINED FROM OR THROUGH
        THE SERVICES CREATES ANY WARRANTY NOT EXPRESSLY STATED HEREIN.
      </p>
    ),
  },
  {
    id: "liability",
    label: "08",
    title: "Limitation of liability",
    body: (
      <p className={paragraph}>
        TO THE MAXIMUM EXTENT PERMITTED BY LAW, THE OPERATORS AND CONTRIBUTORS OF THE COVENANT
        PROJECT WILL NOT BE LIABLE FOR ANY INDIRECT, INCIDENTAL, SPECIAL, CONSEQUENTIAL, EXEMPLARY,
        OR PUNITIVE DAMAGES, OR FOR LOSS OF PROFITS, REVENUES, DATA, OR DIGITAL ASSETS, ARISING
        FROM OR RELATING TO YOUR USE OF OR INABILITY TO USE THE SERVICES, WHETHER BASED ON
        CONTRACT, TORT, STATUTE, OR OTHERWISE, AND EVEN IF ADVISED OF THE POSSIBILITY OF SUCH
        DAMAGES. IN NO EVENT WILL OUR AGGREGATE LIABILITY EXCEED ONE HUNDRED U.S. DOLLARS
        (USD 100).
      </p>
    ),
  },
  {
    id: "indemnification",
    label: "09",
    title: "Indemnification",
    body: (
      <p className={paragraph}>
        You agree to indemnify and hold harmless the operators and contributors of the Covenant
        project from and against any claims, damages, losses, liabilities, costs, and expenses
        (including reasonable attorneys&apos; fees) arising out of or related to your use of the
        Services, your violation of these Terms, or your violation of any third-party right or
        applicable law.
      </p>
    ),
  },
  {
    id: "sanctions",
    label: "10",
    title: "Sanctions and jurisdictional restrictions",
    body: (
      <p className={paragraph}>
        You represent that you are not located in, organized under the laws of, or ordinarily
        resident in any jurisdiction that is the subject of comprehensive U.S., E.U., U.K., or
        U.N. sanctions, and that you are not on any sanctions or denied-parties list. You may not
        use the Services on behalf of any such person or jurisdiction. The Services may be
        unavailable in certain jurisdictions at the operator&apos;s discretion.
      </p>
    ),
  },
  {
    id: "changes",
    label: "11",
    title: "Changes to these Terms",
    body: (
      <p className={paragraph}>
        We may update these Terms from time to time. When we do, the &ldquo;last updated&rdquo;
        date at the top will move and the change will be committed to the public repository at{" "}
        <a
          href="https://github.com/open-covenant/covenant"
          target="_blank"
          rel="noopener noreferrer"
          className={linkClass}
        >
          github.com/open-covenant/covenant
        </a>
        . Material changes will be summarized in the changelog. Your continued use of the Services
        after a change takes effect constitutes acceptance of the updated Terms.
      </p>
    ),
  },
  {
    id: "governing-law",
    label: "12",
    title: "Governing law",
    body: (
      <p className={paragraph}>
        These Terms are governed by the laws of the State of Delaware, United States, without
        regard to its conflict-of-laws principles. The exclusive venue for any dispute arising
        from or relating to the Services or these Terms shall be the state or federal courts
        located in Wilmington, Delaware, and you consent to personal jurisdiction in those courts.
      </p>
    ),
  },
  {
    id: "contact",
    label: "13",
    title: "Contact",
    body: (
      <p className={paragraph}>
        Questions about these Terms or notices required under them:{" "}
        <Link href="/contact" className={linkClass}>
          opencovenant.org/contact
        </Link>
        .
      </p>
    ),
  },
];

export default function TermsPage() {
  return (
    <main className="relative min-h-screen overflow-x-hidden bg-[#030303]">
      <SiteHeader />

      <div className="page-container">
        <div className="mb-16 sm:mb-24">
          <h1 className="mb-3 text-[11px] uppercase tracking-[0.4em] text-neutral-400">
            Terms of Service
          </h1>
          <p className="text-[11px] uppercase tracking-[0.3em] text-neutral-600">
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
