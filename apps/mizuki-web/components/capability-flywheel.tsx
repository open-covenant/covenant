import Link from 'next/link';
import { tractionTargets } from '@/lib/flywheel';
import {
  formatSolLamports,
  formatTime,
  formatUsd,
  formatUsdcAtomic,
  stateLabel,
} from '@/lib/format';
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
      kicker: 'Customer work',
      value: countLabel(metrics.paidJobs, 'paid job'),
      detail: `${formatUsd(metrics.recognizedRevenueUsd)} recognized revenue · ${formatSolLamports(metrics.platformReportedCreatorFeesSentLamports)} platform-reported creator-fee distributions`,
      copy: `${formatUsd(metrics.settledCustomerReceiptsUsd)} in settled customer payments is tracked separately until any related refund obligations are resolved.`,
      href: '/activity',
      link: 'View payment activity',
    },
    {
      id: 'protect',
      number: '02',
      kicker: 'Refund protection',
      value:
        protection.status === 'verified' && protection.finalizedBalanceAtomic !== null
          ? `${formatUsdcAtomic(protection.finalizedBalanceAtomic)} verified reserve balance`
          : protection.finalizedBalanceAtomic === null
            ? 'Reserve records unavailable'
            : `${formatUsdcAtomic(protection.finalizedBalanceAtomic)} reserve balance · needs attention`,
      detail:
        protection.signerOutstandingLiabilityAtomic === null
          ? 'Finalized refund obligations are unavailable'
          : `${formatUsdcAtomic(protection.signerOutstandingLiabilityAtomic)} in refund obligations · ${stateLabel(protection.status)}`,
      copy:
        protection.status === 'verified'
          ? 'Verified means the finalized reserve balance covers outstanding refund obligations and matches the service records.'
          : 'New paid work remains closed while reserve records are missing, stale, inconsistent, or unmatched.',
      href: '/treasury',
      link: 'View reserve evidence',
    },
    {
      id: 'expand',
      number: '03',
      kicker: 'Planned improvements',
      value: `${formatUsd(metrics.plannedImprovementAllocationUsd)} planned for improvements`,
      detail: `${countLabel(metrics.bountiesCreated, 'maintenance bounty', 'maintenance bounties')} · ${countLabel(upgradesInProgress, 'production change')} in progress`,
      copy: treasury.allocationModel.targetsSatisfied
        ? 'This is a planning estimate from service records, not funds held in a wallet or authority to spend. Each bounty requires separate SOL escrow.'
        : 'Published reserve and operating targets have not been met. Amounts shown here remain planning estimates, not wallet balances or spending authority.',
      href: '/bounties',
      link: 'Browse funded bounties',
    },
    {
      id: 'prove',
      number: '04',
      kicker: 'Public evidence',
      value: countLabel(chainReceipts, 'on-chain transaction'),
      detail: `${countLabel(metrics.bountiesReleased, 'bounty payout')} · ${countLabel(capabilityReceipts, 'capability record')} with evidence`,
      copy: 'Payout transactions and production-change evidence are published for verification.',
      href: '/capabilities',
      link: 'View production evidence',
    },
  ];

  return (
    <div className="flywheel-panel">
      <ol
        className="flywheel-stages"
        aria-label="How maintenance revenue supports refunds, bounties, and verified improvements"
      >
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
            <p className="eyebrow">Public operating goals</p>
            <h3 id="traction-title">Published targets and current results</h3>
          </div>
          <p>
            {demo ? 'Example data' : 'Live service data'} · updated {formatTime(metrics.updatedAt)}
          </p>
        </div>
        <ol className="traction-grid">
          {targets.map((target) => (
            <li className={target.met ? 'target-met' : ''} key={target.id}>
              <div className="target-topline">
                <span>{target.label}</span>
                <strong>{target.met ? 'Complete' : 'In progress'}</strong>
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

function countLabel(count: number, singular: string, plural = `${singular}s`): string {
  return `${count} ${count === 1 ? singular : plural}`;
}
