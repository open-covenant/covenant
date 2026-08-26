import type { Metadata } from 'next';
import { pageMetadata } from '@/lib/page-metadata';

export const metadata: Metadata = pageMetadata({
  title: 'Service terms',
  description:
    'Terms for Mizuki paid maintenance jobs, refunds, public records, and contributor bounties.',
  path: '/terms',
});

export default function TermsPage() {
  return (
    <div className="page-shell">
      <section className="page-hero shell">
        <div>
          <p className="eyebrow">Service terms · effective August 24, 2026</p>
          <h1>Terms for paid maintenance and bounties.</h1>
        </div>
        <div className="page-hero-aside">
          <p>
            These Mizuki-specific terms supplement the OpenCovenant Terms of Service and describe
            the operational promise presented before payment or a bounty claim.
          </p>
        </div>
      </section>
      <section className="shell disclosure-page">
        <article className="detail-panel">
          <h2>Governing terms and operator</h2>
          <p>
            Mizuki is a hosted OpenCovenant service. The{' '}
            <a href="https://opencovenant.org/terms" target="_blank" rel="noreferrer">
              OpenCovenant Terms of Service
            </a>{' '}
            govern use of the hosted service, including acceptance, warranties, liability, changes,
            and governing law. These terms add the rules specific to Mizuki jobs and bounties. Send
            private legal notices or account-specific questions through the{' '}
            <a href="https://opencovenant.org/contact" target="_blank" rel="noreferrer">
              OpenCovenant contact channel
            </a>
            .
          </p>
        </article>
        <article className="detail-panel">
          <h2>Authorization and supported work</h2>
          <p>
            Restrict both required GitHub App installations to the selected public repository. A
            maintainer with triage access or higher must authorize the issue in Mizuki Workbench.
            Mizuki may refuse work that is too large, introduces features, changes sensitive
            systems, or falls outside the published scope.
          </p>
        </article>
        <article className="detail-panel">
          <h2>Quote and payment</h2>
          <p>
            A quote is fixed at 2 or 10 USDC, expires after 15 minutes, and binds the repository,
            issue, base revision, file limit, payment recipient, and amount. The paying wallet is
            recorded after payment is confirmed. Do not pay an expired quote or repeat a payment
            whose status is unresolved.
          </p>
        </article>
        <article className="detail-panel">
          <h2>Delivery or refund</h2>
          <p>
            Delivery means Mizuki opens a pull request within the quoted scope after supported
            repository checks pass and a separate AI reviewer approves the patch. If an admission,
            execution, check, review, or GitHub-delivery requirement fails, delivery stops and the
            full quoted USDC amount becomes a refund obligation to the wallet that paid. Solana
            network fees, wallet fees, token price changes, and work outside the authorized issue
            are not included.
          </p>
        </article>
        <article className="detail-panel">
          <h2>Timing and status</h2>
          <p>
            Mizuki does not promise delivery or blockchain finality by a particular clock time. A
            confirmed payment remains recorded as an outstanding service or refund obligation until
            either a qualifying pull request opens or the full refund finalizes. The public job
            record is the source of truth. If it cannot confirm a payment or refund, do not pay
            again; contact Support with the quote or job ID.
          </p>
        </article>
        <article className="detail-panel">
          <h2>Maintainer responsibility</h2>
          <p>
            The separate AI review is not a human review, maintainer approval, or security audit.
            Repository maintainers remain responsible for reviewing, testing, approving, and merging
            every pull request. Mizuki never merges work into a customer repository.
          </p>
        </article>
        <article className="detail-panel">
          <h2>Repository rights and contributions</h2>
          <p>
            Repository owners retain their rights in repository code and issue content. A patch is
            submitted to the selected repository under that repository&apos;s existing license and
            contribution terms. Mizuki does not claim ownership of customer code, and maintainers
            have no obligation to accept or merge a submitted patch.
          </p>
        </article>
        <article className="detail-panel">
          <h2>Bounty claims and payout</h2>
          <p>
            A bounty is published only after the original customer refund and separate SOL escrow
            both finalize. A claim reserves the bounty for 48 hours but does not guarantee payout.
            Payout requires every acceptance criterion, repository checks, separate AI review,
            non-claimant maintainer approval on the exact reviewed commit, and merge before expiry.
          </p>
        </article>
        <article className="detail-panel">
          <h2>Bounty disputes</h2>
          <p>
            The assigned contributor may open a dispute while the claim is active and before
            settlement. Payout pauses while the policy signer applies the published criteria to the
            exact review, approval, merge, deadline, and escrow evidence. The requested outcome,
            final settlement decision, evidence hash and references, timestamps, and transaction are
            published on the bounty record. The signer&apos;s recorded settlement decision
            determines whether the escrow pays the contributor or returns the SOL.
          </p>
        </article>
        <article className="detail-panel">
          <h2>Availability and financial safety</h2>
          <p>
            New quotes and claims can be paused whenever refund protection, settlement, review,
            repository authorization, or deployment checks are unavailable. Existing obligations
            remain recorded, and public records remain available whenever the service can safely
            serve them. The $MIZUKI token does not control customer jobs or create a claim on
            service revenue.
          </p>
        </article>
      </section>
    </div>
  );
}
