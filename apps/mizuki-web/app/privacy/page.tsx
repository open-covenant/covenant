import type { Metadata } from 'next';
import { pageMetadata } from '@/lib/page-metadata';

export const metadata: Metadata = pageMetadata({
  title: 'Privacy and data use',
  description:
    'What Mizuki processes, sends to service providers, retains, and intentionally publishes.',
  path: '/privacy',
});

export default function PrivacyPage() {
  return (
    <div className="page-shell">
      <section className="page-hero shell">
        <div>
          <p className="eyebrow">Privacy and data use · effective August 24, 2026</p>
          <h1>A public-repository service, explained clearly.</h1>
        </div>
        <div className="page-hero-aside">
          <p>
            Do not place secrets, private code, credentials, confidential information, or
            unnecessary personal data in an issue submitted to Mizuki. Public GitHub repositories
            and public transaction records are central to the service.
          </p>
        </div>
      </section>
      <section className="shell disclosure-page">
        <article className="detail-panel">
          <h2>Scope and contact</h2>
          <p>
            This notice supplements the{' '}
            <a href="https://opencovenant.org/privacy" target="_blank" rel="noreferrer">
              OpenCovenant Privacy Policy
            </a>{' '}
            for the hosted Mizuki service. The OpenCovenant operators described there control
            service-side data. Use the{' '}
            <a href="https://opencovenant.org/contact" target="_blank" rel="noreferrer">
              private contact channel
            </a>{' '}
            for privacy requests or account-specific payment questions.
          </p>
        </article>
        <article className="detail-panel">
          <h2>Data Mizuki processes</h2>
          <p>
            Mizuki processes public issue text, public repository code and metadata, generated
            patches, validation output, GitHub identity, wallet address, signed wallet-verification
            messages, quote and job identifiers, service events, and blockchain transaction
            identifiers. Hosting and security providers may also process IP address, user-agent,
            request timing, and security-log data needed to deliver and protect the service.
          </p>
        </article>
        <article className="detail-panel">
          <h2>Why the data is used</h2>
          <p>
            Data is used to confirm repository authorization, price and perform the requested work,
            run checks and review, prevent duplicate payments and claims, deliver pull requests,
            enforce refunds and bounty settlement, resolve disputes, protect the service, and
            publish the evidence described below. Mizuki does not sell personal data or build
            advertising profiles.
          </p>
        </article>
        <article className="detail-panel">
          <h2>AI processing</h2>
          <p>
            Mizuki sends the authorized issue, relevant public repository content, generated patch,
            validation output, and review instructions through UsePod to the selected AI model
            provider. Provider receipts identify the model and request where available. Mizuki does
            not intentionally send wallet private keys, seed phrases, GitHub App private keys, or
            payment credentials to an AI provider. Never place secrets in public issue or repository
            content.
          </p>
        </article>
        <article className="detail-panel">
          <h2>Connected services</h2>
          <p>
            GitHub provides repository access and contributor identity. Solana records payment and
            escrow transactions; x402 is the payment protocol used to request customer payments.
            UsePod routes AI model requests. Render and Cloudflare host, protect, and deliver the
            service. Each provider also processes data under its own terms and privacy policy.
          </p>
        </article>
        <article className="detail-panel">
          <h2>Data published</h2>
          <p>
            Job status, repository and issue references, changed-file names, validation results,
            review decisions, pull requests, refunds, bounty claims, payout and escrow-return
            records, disputes, capability changes, and related transaction identifiers may be
            published as part of the service record. GitHub and Solana records remain subject to
            their own public retention rules.
          </p>
        </article>
        <article className="detail-panel">
          <h2>Sessions and wallet proof</h2>
          <p>
            Contributor sign-in uses GitHub OAuth and an HttpOnly, SameSite=Lax service cookie that
            expires after seven days. Wallet verification signs a message only; it does not
            authorize a transfer. The verified address becomes the payout address for the selected
            bounty claim.
          </p>
          <p>
            WalletConnect connections use Reown&apos;s relay and wallet-discovery services. Reown
            may receive connection metadata needed to establish the session. Mizuki receives the
            public wallet address and signed response, never the wallet&apos;s private key or seed
            phrase.
          </p>
        </article>
        <article className="detail-panel">
          <h2>Retention</h2>
          <p>
            The sign-in cookie expires after seven days, and wallet challenges are short-lived.
            Service records needed for payments, refunds, bounty settlement, disputes, security, and
            public auditability are retained while the service keeps its operational record; no
            shorter automatic deletion schedule is currently promised. Public GitHub and Solana
            records may remain available indefinitely outside Mizuki&apos;s control.
          </p>
        </article>
        <article className="detail-panel">
          <h2>Your choices and requests</h2>
          <p>
            You can choose not to submit an issue, connect a wallet, pay a quote, or claim a bounty.
            Contact OpenCovenant privately to request access, correction, or deletion of
            service-side personal data. A request may be limited where records must be retained for
            payment, dispute, security, or legal obligations, and OpenCovenant cannot erase records
            controlled by GitHub, Solana, or another public network.
          </p>
        </article>
        <article className="detail-panel">
          <h2>Security reports</h2>
          <p>
            Do not send a vulnerability through a public support issue. Use Mizuki&apos;s private
            security-reporting channel for suspected security problems.
          </p>
          <a className="button button-secondary" href="/security">
            View security reporting
          </a>
        </article>
      </section>
    </div>
  );
}
