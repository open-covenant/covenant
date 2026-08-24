import type { Metadata } from 'next';
import { pageMetadata } from '@/lib/page-metadata';

export const metadata: Metadata = pageMetadata({
  title: 'Security',
  description: 'Mizuki security boundaries, limitations, and responsible disclosure channel.',
  path: '/security',
});

export default function SecurityPage() {
  return (
    <div className="page-shell">
      <section className="page-hero shell">
        <div>
          <p className="eyebrow">Security</p>
          <h1>Strict scope and separated authority.</h1>
        </div>
        <div className="page-hero-aside">
          <p>
            Mizuki does not accept vulnerability remediation, authentication changes, secret
            handling, payment code, custody code, or production incident response as paid jobs.
          </p>
        </div>
      </section>
      <section className="shell disclosure-page">
        <article className="detail-panel">
          <h2>Repository access</h2>
          <p>
            The maintenance App is installed per repository and uses short-lived, repository-scoped
            credentials. Mizuki rechecks the installation, authorization label, issue text, and base
            revision before opening a pull request. Maintainers retain merge control.
          </p>
        </article>
        <article className="detail-panel">
          <h2>Financial separation</h2>
          <p>
            Mizuki&apos;s coding service cannot access refund or bounty funds. A separate policy
            signer verifies the original payment and enforces refund and escrow rules. Missing or
            stale evidence closes new paid work.
          </p>
        </article>
        <article className="detail-panel">
          <h2>Review limitation</h2>
          <p>
            The separate AI review is a product quality check. It is not a penetration test, formal
            verification, human review, maintainer approval, or security audit.
          </p>
        </article>
        <article className="detail-panel">
          <h2>Report a vulnerability</h2>
          <p>
            Do not publish suspected vulnerabilities in a public issue. Use the repository&apos;s
            private security-advisory channel or email security@opencovenant.org so maintainers can
            investigate before disclosure.
          </p>
          <div className="completion-links">
            <a
              className="button button-secondary"
              href="https://github.com/open-covenant/covenant/security/advisories/new"
              target="_blank"
              rel="noreferrer"
            >
              Open a private security advisory <span aria-hidden="true">↗</span>
            </a>
            <a className="button button-secondary" href="mailto:security@opencovenant.org">
              Email security@opencovenant.org
            </a>
          </div>
        </article>
      </section>
    </div>
  );
}
