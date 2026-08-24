import type { Metadata } from 'next';
import { CapabilityReceipts } from '@/components/capability-receipts';
import { DataError, DemoNotice, EmptyState } from '@/components/data-state';
import { StatusBadge } from '@/components/status-badge';
import { getCapabilities } from '@/lib/api';
import { formatTime } from '@/lib/format';
import type { CapabilityState } from '@/lib/types';

export const dynamic = 'force-dynamic';

export const metadata: Metadata = {
  title: 'Capability record',
  description:
    'See the benchmarks, pull requests, reviews, and deployments behind Mizuki’s claimed improvements.',
};

const stateOrder: CapabilityState[] = [
  'missing',
  'proposed',
  'funded',
  'implementing',
  'validating',
  'active',
  'degraded',
  'retired',
];

export default async function CapabilitiesPage() {
  const result = await getCapabilities();
  return (
    <div className="page-shell">
      <section className="page-hero shell">
        <div>
          <p className="eyebrow">
            Evidence-backed evolution {result.status !== 'error' && result.demo && <DemoNotice />}
          </p>
          <h1>Mizuki’s body is the code he can prove.</h1>
        </div>
        <div className="page-hero-aside">
          <p>
            A capability moves forward only when its benchmark, implementation, independent review,
            and deployment evidence agree. Separate external authorities sign the proposal,
            benchmark, and review before the updater can change production.
          </p>
          <div className="capability-legend">
            {stateOrder.map((state) => (
              <StatusBadge state={state} key={state} />
            ))}
          </div>
        </div>
      </section>
      <section className="shell capability-page-section">
        {result.status === 'error' ? (
          <DataError title="Capability record unavailable" detail={result.error} />
        ) : result.status === 'empty' ? (
          <EmptyState title="No capability evidence published">
            The first validated upgrade will establish a public baseline.
          </EmptyState>
        ) : (
          <div className="capability-board">
            {stateOrder.map((state) => {
              const items = result.data.filter((capability) => capability.state === state);
              return (
                <section key={state}>
                  <div className="capability-column-heading">
                    <StatusBadge state={state} />
                    <span>{items.length}</span>
                  </div>
                  <div className="capability-column-cards">
                    {items.map((capability) => (
                      <article key={capability.id}>
                        <p className="eyebrow">{capability.category}</p>
                        <h2>{capability.name}</h2>
                        <p>{capability.description}</p>
                        {(capability.benchmarkBefore !== undefined ||
                          capability.benchmarkAfter !== undefined) && (
                          <div className="capability-benchmark">
                            <span>Benchmark</span>
                            <div>
                              <span>{capability.benchmarkBefore ?? '—'}</span>
                              <span aria-hidden="true">→</span>
                              <strong>{capability.benchmarkAfter ?? 'pending'}</strong>
                              <small>{capability.benchmarkUnit}</small>
                            </div>
                          </div>
                        )}
                        <CapabilityReceipts capability={capability} />
                        <div className="capability-card-footer">
                          <span>{formatTime(capability.updatedAt)}</span>
                          {capability.handoffUrl && (
                            <a
                              href={`/api/mizuki${capability.handoffUrl}`}
                              target="_blank"
                              rel="noreferrer"
                            >
                              Authority handoff <span aria-hidden="true">↗</span>
                            </a>
                          )}
                        </div>
                      </article>
                    ))}
                    {items.length === 0 && <p className="column-empty">Nothing in this state.</p>}
                  </div>
                </section>
              );
            })}
          </div>
        )}
      </section>
    </div>
  );
}
