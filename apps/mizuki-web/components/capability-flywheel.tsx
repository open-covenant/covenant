import Link from 'next/link';
import { tractionTargets } from '@/lib/flywheel';
import { formatSolLamports, formatTime, formatUsd, formatUsdcAtomic } from '@/lib/format';
import type { Capability, Metrics, Treasury } from '@/lib/types';

type Props = {
  metrics: Metrics;
  treasury: Treasury;
  capabilities: Capability[];
  demo?: boolean;
};

export function CapabilityFlywheel({ metrics, treasury, capabilities, demo = false }: Props) {
  const upgradesInProgress = capabilities.filter((capability) =>
    ['proposed', 'funded', 'implementing', 'validating'].includes(capability.state),
  ).length;
  const capabilityReceipts = capabilities.filter(hasCapabilityEvidence).length;
  const chainReceipts = treasury.ledger.filter((entry) => entry.transaction).length;
  const targets = tractionTargets(metrics);
  const protection = treasury.refundProtection;

  const stages = [
    {
      id: 'earn',
      number: '01',
      kicker: 'Earn',
      value: `${metrics.paidJobs} paid jobs`,
      detail: `${formatUsd(metrics.recognizedRevenueUsd)} recognized revenue · ${formatSolLamports(metrics.platformReportedCreatorFeesSentLamports)} platform-reported creator-fee distributions`,
      copy: `${formatUsd(metrics.settledCustomerReceiptsUsd)} settled customer receipts are tracked separately until refund liabilities are discharged.`,
      href: '/activity',
      link: 'Inspect inflows',
    },
    {
      id: 'protect',
      number: '02',
      kicker: 'Protect',
      value:
        protection.status === 'verified' && protection.finalizedBalanceAtomic !== null
          ? `${formatUsdcAtomic(protection.finalizedBalanceAtomic)} verified custody`
          : protection.finalizedBalanceAtomic === null
            ? 'Signer evidence unavailable'
            : `${formatUsdcAtomic(protection.finalizedBalanceAtomic)} signer balance · degraded`,
      detail:
        protection.signerOutstandingLiabilityAtomic === null
          ? 'No fresh finalized refund evidence'
          : `${formatUsdcAtomic(protection.signerOutstandingLiabilityAtomic)} signer liabilities · ${protection.status}`,
      copy:
        protection.status === 'verified'
          ? 'Finalized signer custody backs the reconciled refund liabilities shown here.'
          : 'Protection is not marked verified while evidence is missing, stale, incoherent, or unreconciled.',
      href: '/treasury',
      link: 'Audit the waterfall',
    },
    {
      id: 'expand',
      number: '03',
      kicker: 'Expand',
      value: `${formatUsd(metrics.plannedImprovementAllocationUsd)} planned improvement allocation`,
      detail: `${metrics.bountiesCreated} rescue bounties · ${upgradesInProgress} upgrades in progress`,
      copy: treasury.allocationModel.targetsSatisfied
        ? 'The application ledger models this earmark; it is not wallet custody or spend authority. Rescue bounties use separate signer-controlled SOL escrow.'
        : 'The application-ledger targets are not filled. No modeled allocation is presented as custody or spend authority.',
      href: '/bounties',
      link: 'Open rescue board',
    },
    {
      id: 'prove',
      number: '04',
      kicker: 'Prove',
      value: `${chainReceipts} on-chain receipts`,
      detail: `${metrics.bountiesReleased} rescue payouts · ${capabilityReceipts} capability records with evidence`,
      copy: 'Payout transactions and upgrade evidence close the loop in public.',
      href: '/capabilities',
      link: 'Review capability evidence',
    },
  ];

  return (
    <div className="flywheel-panel">
      <ol className="flywheel-stages" aria-label="Mizuki capability funding flywheel">
        {stages.map((stage, index) => (
          <li key={stage.id}>
            <div className="flywheel-stage-heading">
              <span>{stage.number}</span>
              <strong>{stage.kicker}</strong>
            </div>
            <p className="flywheel-value">{stage.value}</p>
            <p className="flywheel-detail">{stage.detail}</p>
            <p className="flywheel-copy">{stage.copy}</p>
            <Link href={stage.href} className="flywheel-link">
              {stage.link} <span aria-hidden="true">↗</span>
            </Link>
            {index < stages.length - 1 && (
              <span className="flywheel-connector" aria-hidden="true">
                →
              </span>
            )}
          </li>
        ))}
      </ol>

      <section className="traction-board" aria-labelledby="traction-title">
        <div className="traction-heading">
          <div>
            <p className="eyebrow">Launch proof</p>
            <h3 id="traction-title">The targets are public. So is the gap.</h3>
          </div>
          <p>
            {demo ? 'Illustrative fixture' : 'Live backend records only'} · updated{' '}
            {formatTime(metrics.updatedAt)}
          </p>
        </div>
        <ol className="traction-grid">
          {targets.map((target) => (
            <li className={target.met ? 'target-met' : ''} key={target.id}>
              <div className="target-topline">
                <span>{target.label}</span>
                <strong>{target.met ? 'Met' : 'Open'}</strong>
              </div>
              <div className="target-value">
                <strong>{target.value}</strong>
                <span>/ {target.target}</span>
              </div>
              <span
                className="target-track"
                role="progressbar"
                aria-label={`${target.label}: ${target.value} of ${target.target}`}
                aria-valuemin={0}
                aria-valuemax={100}
                aria-valuenow={Math.round(target.progress)}
              >
                <span style={{ width: `${target.progress}%` }} />
              </span>
              <p>{target.detail}</p>
            </li>
          ))}
        </ol>
      </section>
    </div>
  );
}

function hasCapabilityEvidence(capability: Capability): boolean {
  if (capability.evidenceUrl) return true;
  return Object.values(capability.evidence ?? {}).some(Boolean);
}
