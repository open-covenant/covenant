import type { Metadata } from "next";
import { SiteFooter } from "../SiteFooter";
import { SiteHeader } from "../SiteHeader";
import { GITHUB_URL } from "../_brand";

const TITLE = "Security";
const DESCRIPTION =
  "How to report a vulnerability in Covenant, what's in scope, and the security posture: Apache-2.0 source, hash-chained audit, and on-chain attestation anyone can verify.";

export const metadata: Metadata = {
  title: TITLE,
  description: DESCRIPTION,
  alternates: { canonical: "/security" },
  openGraph: { type: "website", url: "https://opencovenant.org/security", title: TITLE, description: DESCRIPTION },
  twitter: { card: "summary_large_image", site: "@OpenCovenant", creator: "@OpenCovenant", title: TITLE, description: DESCRIPTION },
};

const eyebrow = "font-mono text-[11px] uppercase tracking-[0.3em] text-neutral-400";
const paragraph = "text-[13px] leading-relaxed text-neutral-300 sm:text-[14px]";
const link =
  "underline decoration-neutral-700 underline-offset-4 transition-colors hover:text-neutral-50 hover:decoration-neutral-300";

export default function SecurityPage() {
  return (
    <>
      <SiteHeader />
      <main className="mx-auto w-full max-w-7xl px-5 pb-24 pt-14 sm:px-8">
        <p className={eyebrow}>disclosure &middot; scope &middot; posture</p>
        <h1 className="mt-4 text-2xl font-extralight tracking-[0.18em] text-neutral-50 sm:text-3xl">Security</h1>
        <p className={`${paragraph} mt-5 max-w-2xl`}>
          Covenant governs agents that hold keys and move value, so its security model is the product, not
          a footnote. The source is Apache-2.0 and public, every privileged action is hash-chained into a
          signed audit, and audit roots are anchored on-chain where anyone can verify them. The point is
          that you never have to take our word for it.
        </p>

        <section className="mt-12">
          <p className={eyebrow}>report a vulnerability</p>
          <p className={`${paragraph} mt-3 max-w-2xl`}>
            Email{" "}
            <a className={link} href="mailto:contact@opencovenant.org">
              contact@opencovenant.org
            </a>{" "}
            with a description and reproduction steps. For sensitive reports, encrypt to the PGP public key
            published in the repository at{" "}
            <a className={link} href={`${GITHUB_URL}/blob/main/SECURITY-PGP-PUBLIC.asc`}>
              SECURITY-PGP-PUBLIC.asc
            </a>
            . Please give us a reasonable window to ship a fix before public disclosure. We do not pursue
            researchers acting in good faith.
          </p>
        </section>

        <section className="mt-12">
          <p className={eyebrow}>in scope</p>
          <ul className={`${paragraph} mt-3 max-w-2xl list-disc space-y-1.5 pl-5 text-neutral-400`}>
            <li>The Covenant daemon (covenantd): intent dispatch, the permissions and capability engine, memory, and the IPC and HTTP gateways.</li>
            <li>The Solana programs: settlement and staking, and the on-chain attestation and identity records.</li>
            <li>The hosted trust MCP at mcp.opencovenant.org and the sandbox at sandbox.opencovenant.org.</li>
            <li>Signature verification, canonicalization, and anything that could forge or replay a receipt.</li>
          </ul>
          <p className={`${paragraph} mt-3 max-w-2xl text-neutral-500`}>
            Out of scope: findings that require a compromised host or a leaked operator key, volumetric
            denial of service, and issues in third-party services Covenant integrates with rather than
            operates.
          </p>
        </section>

        <section className="mt-12">
          <p className={eyebrow}>posture &middot; verify it as a property</p>
          <p className={`${paragraph} mt-3 max-w-2xl`}>
            A reckless action is refused before it reaches a wallet, a malicious signature is rejected
            before it is signed, and every permitted action settles with a receipt anyone can check. The
            mechanics are documented:{" "}
            <a className={link} href="/docs/security">
              runtime sandbox and gateway
            </a>
            ,{" "}
            <a className={link} href="/docs/audit">
              the hash-chained audit
            </a>
            ,{" "}
            <a className={link} href="/docs/audit-integrity">
              audit integrity
            </a>
            , and{" "}
            <a className={link} href="/docs/capabilities">
              the capability model
            </a>
            .
          </p>
        </section>

        <p className={`${paragraph} mt-14 text-[11.5px] text-neutral-600`}>
          Covenant is open source under Apache-2.0. There is no formal bug-bounty program at this time;
          we acknowledge reporters who ask to be credited.
        </p>
      </main>
      <SiteFooter className="pb-8" />
    </>
  );
}
