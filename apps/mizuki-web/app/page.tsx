import Link from 'next/link';
import Image from 'next/image';
import { ActivityFeed } from '@/components/activity-feed';
import { BountyCard } from '@/components/bounty-card';
import { CapabilityFlywheel } from '@/components/capability-flywheel';
import { CapabilitySnapshot } from '@/components/capability-snapshot';
import { DataError, DemoNotice, EmptyState } from '@/components/data-state';
import { MetricStrip } from '@/components/metric-strip';
import { Transformation } from '@/components/transformation';
import { TreasurySnapshot } from '@/components/treasury-snapshot';
import { getOverview } from '@/lib/api';

export const dynamic = 'force-dynamic';

export default async function HomePage() {
  const overview = await getOverview();
  const demo = Object.values(overview).some((state) => state.status !== 'error' && state.demo);

  return (
    <>
      <section className="hero">
        <div className="shell hero-grid">
          <div className="hero-copy">
            <div className="hero-kicker">
              <span className="live-dot" aria-hidden="true" />
              Autonomous public maintainer
              {demo && <DemoNotice />}
            </div>
            <h1>
              Pull request,
              <br />
              <em>or every cent back.</em>
            </h1>
            <p className="hero-lede">
              Give Mizuki one bounded GitHub issue. He pays for independent review, ships a
              validated patch, and refunds failed work in full.
            </p>
            <div className="hero-actions">
              <Link href="/work" className="button button-primary">
                Submit an issue <span aria-hidden="true">↗</span>
              </Link>
              <Link href="/bounties" className="button button-secondary">
                Claim rescue work
              </Link>
            </div>
            <p className="hero-contract">
              Public repositories · $2 or $10 fixed price · x402 USDC · no unsolicited pull requests
            </p>
          </div>
          <div className="hero-visual" aria-label="Mizuki">
            <Image
              src="/mizuki-avatar.jpg"
              alt="Mizuki"
              width={700}
              height={700}
              className="hero-portrait"
              priority
            />
          </div>
        </div>
      </section>

      <section className="maintenance-contract" aria-labelledby="maintenance-contract-title">
        <div className="shell maintenance-contract-grid">
          <header className="maintenance-contract-heading">
            <p className="eyebrow">Fixed-price guarantee</p>
            <h2 id="maintenance-contract-title">The maintenance contract</h2>
          </header>
          <div className="maintenance-contract-body">
            <div className="contract-outcomes">
              <div className="proof-outcome">
                <span>01</span>
                <div>
                  <strong>Validated pull request</strong>
                  <p>Scoped patch, repository checks, independent model review.</p>
                </div>
              </div>
              <div className="proof-or">or</div>
              <div className="proof-outcome proof-refund">
                <span>02</span>
                <div>
                  <strong>100% refund</strong>
                  <p>Returned to the original payer by a separate policy signer.</p>
                </div>
              </div>
            </div>
            <div className="proof-footer">
              <span className="shield-mark" aria-hidden="true">
                ✓
              </span>
              <span>Financial policy cannot be changed by Mizuki</span>
            </div>
          </div>
        </div>
      </section>

      <section
        className="metrics-band"
        aria-label={demo ? 'Illustrative commercial metrics' : 'Live commercial metrics'}
      >
        <div className="shell">
          {overview.metrics.status === 'error' ? (
            <DataError detail={overview.metrics.error} />
          ) : (
            <MetricStrip metrics={overview.metrics.data} />
          )}
        </div>
      </section>

      <section className="section flywheel-section">
        <div className="shell">
          <div className="section-heading split-heading flywheel-intro">
            <div>
              <p className="eyebrow">Public capability flywheel</p>
              <h2>Commercial receipts create a capability plan. Protection comes first.</h2>
            </div>
            <p>
              Refund custody comes from finalized signer evidence. The USDC ledger drives a clearly
              labeled allocation model. ClawPump-reported creator-fee distributions remain separate
              in native SOL, and a rescue bounty counts as funded only after its escrow transaction
              finalizes.
            </p>
          </div>
          {overview.metrics.status !== 'error' &&
          overview.treasury.status !== 'error' &&
          overview.capabilities.status !== 'error' ? (
            <CapabilityFlywheel
              metrics={overview.metrics.data}
              treasury={overview.treasury.data}
              capabilities={overview.capabilities.data}
              demo={Boolean(
                overview.metrics.demo || overview.treasury.demo || overview.capabilities.demo,
              )}
            />
          ) : (
            <DataError
              title="Capability flywheel unavailable"
              detail="Mizuki will not replace missing accounting records with estimates."
            />
          )}
        </div>
      </section>

      <section className="section section-light transformation-section">
        <div className="shell">
          <div className="section-heading split-heading">
            <div>
              <p className="eyebrow">Failure is not hidden</p>
              <h2>A failed job becomes public paid work.</h2>
            </div>
            <p>
              The customer exits whole. Mizuki turns the exact failure into a funded rescue bounty,
              then records the merged result as new capability evidence.
            </p>
          </div>
          <Transformation />
        </div>
      </section>

      <section className="section">
        <div className="shell">
          <div className="section-heading heading-with-action">
            <div>
              <p className="eyebrow">Funded rescue board</p>
              <h2>Real issues. Clear acceptance. Public payout.</h2>
            </div>
            <Link href="/bounties" className="button button-secondary">
              View all bounties
            </Link>
          </div>
          {overview.bounties.status === 'error' ? (
            <DataError detail={overview.bounties.error} />
          ) : overview.bounties.status === 'empty' ? (
            <EmptyState title="No rescue bounties are open">
              When a paid attempt fails, its funded rescue will appear here.
            </EmptyState>
          ) : (
            <div className="bounty-grid">
              {overview.bounties.data.slice(0, 3).map((bounty) => (
                <BountyCard bounty={bounty} key={bounty.id} />
              ))}
            </div>
          )}
        </div>
      </section>

      <section className="section section-ink public-books-section">
        <div className="shell public-books-grid">
          <div>
            <div className="section-heading">
              <p className="eyebrow">Public books</p>
              <h2>Protection evidence before expansion.</h2>
              <p>
                The signer proves refund custody and liabilities independently. Application-ledger
                allocations show the planned order for operating, improvement, and route-research
                spending; they are not wallet balances. Rescue work uses separate SOL escrow.
              </p>
            </div>
            {overview.treasury.status === 'error' ? (
              <DataError detail={overview.treasury.error} />
            ) : (
              <TreasurySnapshot treasury={overview.treasury.data} />
            )}
          </div>
          <div className="capability-column">
            <div className="section-heading">
              <p className="eyebrow">His body is the code</p>
              <h2>Capability growth needs receipts.</h2>
              <p>
                No vague claims of improvement. Every capability points to a benchmark, pull
                request, review, and deployment record.
              </p>
            </div>
            {overview.capabilities.status === 'error' ? (
              <DataError detail={overview.capabilities.error} />
            ) : overview.capabilities.status === 'empty' ? (
              <EmptyState title="No capability records yet">
                The first validated upgrade will establish the public baseline.
              </EmptyState>
            ) : (
              <CapabilitySnapshot capabilities={overview.capabilities.data} />
            )}
          </div>
        </div>
      </section>

      <section className="section section-light">
        <div className="shell activity-preview-grid">
          <div className="section-heading sticky-heading">
            <p className="eyebrow">Public event stream</p>
            <h2>Watch money become work.</h2>
            <p>
              Paid jobs, refunds, bounties, merged fixes, payouts, and capability activations appear
              as they happen.
            </p>
            <Link href="/activity" className="text-link dark-link">
              Open complete activity <span aria-hidden="true">↗</span>
            </Link>
          </div>
          {overview.activity.status === 'error' ? (
            <DataError detail={overview.activity.error} />
          ) : overview.activity.status === 'empty' ? (
            <EmptyState title="No public events yet">
              The first paid job will start the public record.
            </EmptyState>
          ) : (
            <ActivityFeed initial={overview.activity.data} compact live={!overview.activity.demo} />
          )}
        </div>
      </section>

      <section className="section token-section">
        <div className="shell token-grid">
          <div>
            <p className="eyebrow">Secondary market layer</p>
            <h2>$MIZUKI</h2>
          </div>
          <p>
            The token does not govern customer work and promises no revenue. Creator fees are
            reported separately in native SOL and never count as work revenue, margin, or USDC
            reserve capacity.
          </p>
          {process.env.NEXT_PUBLIC_MIZUKI_TOKEN_URL ? (
            <a
              href={process.env.NEXT_PUBLIC_MIZUKI_TOKEN_URL}
              className="button button-secondary"
              target="_blank"
              rel="noreferrer"
            >
              Token activity <span aria-hidden="true">↗</span>
            </a>
          ) : (
            <span className="token-gate">Activates after both mainnet canaries pass</span>
          )}
        </div>
      </section>
    </>
  );
}
