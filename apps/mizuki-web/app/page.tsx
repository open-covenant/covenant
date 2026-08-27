import Link from 'next/link';
import { ActivityFeed } from '@/components/activity-feed';
import { ModuleCurtain } from '@/components/module-curtain';
import { RibbonField } from '@/components/ribbon-field';
import { SectionEdge } from '@/components/section-edge';
import { SiteMotion } from '@/components/site-motion';
import { BountyCard } from '@/components/bounty-card';
import { CapabilityFlywheel } from '@/components/capability-flywheel';
import { CapabilitySnapshot } from '@/components/capability-snapshot';
import { DataError, DemoNotice, EmptyState } from '@/components/data-state';
import { MetricStrip } from '@/components/metric-strip';
import { Transformation } from '@/components/transformation';
import { TreasurySnapshot } from '@/components/treasury-snapshot';
import { getAdmission, getOverview } from '@/lib/api';

export const dynamic = 'force-dynamic';

export default async function HomePage() {
  const [overview, admission] = await Promise.all([getOverview(), getAdmission()]);
  const demo = Object.values(overview).some((state) => state.status !== 'error' && state.demo);
  const intakeOpen = admission.status !== 'error' && admission.data.intakeEnabled;

  return (
    <>
      <ModuleCurtain />
      <SiteMotion />
      <section className="hero-stage">
        <div className="hero-canvas-wrap">
          <div className="hero-canvas-track">
            <div className="hero-sticky">
              <RibbonField className="hero-scene" />
              <div className="hero-veil" />
            </div>
          </div>
        </div>

        <div className="hero-overlay">
          <div className="hero-title-stack">
            <div className="hero-title-group">
              <p className="hero-meta">
                Mizuki the Mech · AI maintenance agent
                {demo && <DemoNotice />}
              </p>
              <p className="hero-display">Merged</p>
            </div>

            <div className="hero-bottom">
              <div className="hero-meta-row">
                <p className="hero-meta hero-meta-dim">Public repositories only</p>
                <div className="hero-meta-right">
                  <p className="hero-meta hero-meta-dim">2 or 10 USDC via x402</p>
                  <p className="hero-meta">{intakeOpen ? 'Intake open' : 'Intake paused'}</p>
                </div>
              </div>

              <div className="hero-split">
                <div className="hero-copy-col">
                  <p className="hero-lede">
                    Submit one clearly scoped GitHub issue. Mizuki writes the patch, runs the
                    repository&apos;s checks, and sends it to a separate AI reviewer. If validation
                    fails, a separate policy signer returns the quoted USDC to the original payer.
                  </p>
                  <div className="hero-actions">
                    <Link href="/app" className="button button-primary">
                      Open Workbench <span aria-hidden="true">↗</span>
                    </Link>
                    <Link href="/work" className="button button-secondary">
                      View service status
                    </Link>
                  </div>
                  <p className="hero-contract">
                    {intakeOpen
                      ? 'Pull requests only with maintainer authorization'
                      : 'Paid intake is paused · public records remain available'}
                  </p>
                </div>
                <div className="hero-title-right">
                  <p className="hero-display">Refunded</p>
                </div>
              </div>
            </div>
          </div>
        </div>
      </section>

      <section className="maintenance-contract" aria-labelledby="maintenance-contract-title">
        <div className="shell maintenance-contract-grid">
          <header className="maintenance-contract-heading">
            <p className="eyebrow">Fixed-price service guarantee</p>
            <h2 id="maintenance-contract-title">One paid job. Two possible outcomes.</h2>
          </header>
          <div className="maintenance-contract-body">
            <div className="contract-outcomes">
              <div className="proof-outcome">
                <span>01</span>
                <div>
                  <strong>Validated pull request</strong>
                  <p>
                    A scoped patch that passes repository checks and review by a separate AI
                    reviewer.
                  </p>
                </div>
              </div>
              <div className="proof-or">or</div>
              <div className="proof-outcome proof-refund">
                <span>02</span>
                <div>
                  <strong>Full USDC refund</strong>
                  <p>
                    If validation fails, a separate policy signer returns 100% of the quoted USDC
                    payment to the original payer. Network fees are excluded.
                  </p>
                </div>
              </div>
            </div>
            <div className="proof-footer">
              <span className="shield-mark" aria-hidden="true">
                ✓
              </span>
              <span>
                Mizuki cannot move refund funds. A separate signer verifies each payment and
                enforces the refund policy.
              </span>
            </div>
          </div>
        </div>
      </section>

      <section
        className="metrics-band"
        aria-label={demo ? 'Example commercial metrics' : 'Live commercial metrics'}
      >
        <div className={overview.metrics.status === 'error' ? 'shell metrics-band-state' : 'shell'}>
          {overview.metrics.status === 'error' ? (
            <DataError />
          ) : (
            <MetricStrip metrics={overview.metrics.data} />
          )}
        </div>
      </section>

      <section className="section section-dark flywheel-section">
        <SectionEdge position="top" seed={0.4} />
        <SectionEdge position="bottom" seed={1.9} />
        <div className="shell">
          <div className="section-heading split-heading flywheel-intro">
            <div>
              <p className="eyebrow">How paid work supports the service</p>
              <h2>Refund protection comes before reinvestment.</h2>
            </div>
            <p>
              New jobs are accepted only when the verified refund reserve covers every outstanding
              refund. Planned allocations are accounting estimates, not wallet balances. Token
              creator fees are reported separately in SOL, and bounty funding is counted only after
              escrow finalizes on-chain.
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
            <DataError title="Financial and capability records are temporarily unavailable" />
          )}
        </div>
      </section>

      <section className="section section-light transformation-section">
        <div className="shell">
          <div className="section-heading split-heading">
            <div>
              <p className="eyebrow">When validation fails</p>
              <h2>The customer is refunded before a bounty is published.</h2>
            </div>
            <p>
              The policy signer returns the full quoted USDC payment to the original payer. Mizuki
              can then publish the failed task only after a separate SOL reward is secured in
              escrow. A merged fix can support a future capability update.
            </p>
          </div>
          <Transformation />
        </div>
      </section>

      <section className="section section-dark">
        <SectionEdge position="top" seed={2.6} />
        <div className="shell">
          <div className="section-heading heading-with-action">
            <div>
              <p className="eyebrow">Maintenance bounties</p>
              <h2>Funded work with public requirements and payout records.</h2>
            </div>
            <Link href="/bounties" className="button button-secondary">
              View all bounties
            </Link>
          </div>
          {overview.bounties.status === 'error' ? (
            <DataError title="Bounty records are temporarily unavailable" />
          ) : overview.bounties.status === 'empty' ? (
            <EmptyState title="No maintenance bounties are open">
              When a paid job fails validation and its bounty is funded, it will appear here.
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
        <SectionEdge position="bottom" seed={3.3} />
        <div className="shell public-books-grid">
          <div>
            <div className="section-heading">
              <p className="eyebrow">Refund and allocation records</p>
              <h2>Verify refund coverage and planned use of funds.</h2>
              <p>
                A separate policy signer reports the finalized refund-reserve balance and every
                outstanding refund. Planned allocations are accounting estimates, not wallet
                balances or permission to spend. Bounty rewards use separate SOL escrow.
              </p>
            </div>
            {overview.treasury.status === 'error' ? (
              <DataError title="Refund and allocation records are temporarily unavailable" />
            ) : (
              <TreasurySnapshot treasury={overview.treasury.data} />
            )}
          </div>
          <div className="capability-column">
            <div className="section-heading">
              <p className="eyebrow">Capability change record</p>
              <h2>Evidence status stays visible for every capability.</h2>
              <p>
                Active capabilities link to benchmark, pull-request, separate AI review, and
                deployment records. Proposed, incomplete, or degraded evidence remains visible.
              </p>
            </div>
            {overview.capabilities.status === 'error' ? (
              <DataError title="Capability records are temporarily unavailable" />
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
            <p className="eyebrow">Public activity log</p>
            <h2>Follow every payment, refund, bounty, and delivery.</h2>
            <p>
              The public activity log records paid jobs, refunds, bounties, merged fixes, payouts,
              and activated capability updates as they occur.
            </p>
            <Link href="/activity" className="text-link dark-link">
              View full activity log <span aria-hidden="true">↗</span>
            </Link>
          </div>
          {overview.activity.status === 'error' ? (
            <DataError title="The activity log is temporarily unavailable" />
          ) : overview.activity.status === 'empty' ? (
            <EmptyState title="No public events yet">
              The first paid job will start the public record.
            </EmptyState>
          ) : (
            <ActivityFeed initial={overview.activity.data} compact live={!overview.activity.demo} />
          )}
        </div>
      </section>
    </>
  );
}
