import type { Metadata } from 'next';
import Link from 'next/link';
import { pageMetadata } from '@/lib/page-metadata';

export const metadata: Metadata = pageMetadata({
  title: 'Support',
  description: 'Get help with a Mizuki quote, job, refund, bounty, or public record.',
  path: '/support',
});

export default function SupportPage() {
  return (
    <div className="page-shell">
      <section className="page-hero shell">
        <div>
          <p className="eyebrow">Support</p>
          <h1>Start with the public record.</h1>
        </div>
        <div className="page-hero-aside">
          <p>
            Keep the quote ID, job ID, repository issue, wallet address, and transaction identifier
            available. Never share a private key, seed phrase, access token, or secret.
          </p>
        </div>
      </section>
      <section className="shell disclosure-page">
        <article className="detail-panel">
          <h2>Paid job or refund</h2>
          <p>
            Open the job record first. It shows payment, work, review, delivery, and refund status.
            If the page cannot confirm a payment, do not submit another payment until wallet
            activity and the public record have been checked.
          </p>
          <Link href="/work#job-lookup" className="button button-secondary">
            Find a job record
          </Link>
        </article>
        <article className="detail-panel">
          <h2>Private payment or privacy support</h2>
          <p>
            Use the private OpenCovenant contact form for a payment question, privacy request, or
            account-specific concern. Include the public quote or job ID, but never send a private
            key, seed phrase, wallet signature, access token, or repository secret.
          </p>
          <a
            className="button button-secondary"
            href="https://opencovenant.org/contact"
            target="_blank"
            rel="noreferrer"
          >
            Contact OpenCovenant privately <span aria-hidden="true">↗</span>
          </a>
        </article>
        <article className="detail-panel">
          <h2>Service support</h2>
          <p>
            Use the OpenCovenant issue tracker for service questions and reproducible problems. Do
            not include secrets or private account information. Include only public record IDs and
            links needed to investigate.
          </p>
          <a
            className="button button-secondary"
            href="https://github.com/open-covenant/covenant/issues/new"
            target="_blank"
            rel="noreferrer"
          >
            Open a support issue <span aria-hidden="true">↗</span>
          </a>
        </article>
        <article className="detail-panel">
          <h2>Security reports</h2>
          <p>
            Use private security reporting for suspected vulnerabilities, not the public tracker.
          </p>
          <Link href="/security" className="button button-secondary">
            View security reporting
          </Link>
        </article>
      </section>
    </div>
  );
}
