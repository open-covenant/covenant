import type { Metadata } from 'next';
import { CapabilityReceipts } from '@/components/capability-receipts';
import { DataError, DemoNotice, EmptyState } from '@/components/data-state';
import { StatusBadge } from '@/components/status-badge';
import { getCapabilities } from '@/lib/api';
import { formatTime } from '@/lib/format';
import type { CapabilityState } from '@/lib/types';

export const dynamic = 'force-dynamic';

export const metadata: Metadata = {
  title: 'Production change record',
  description:
    'Review benchmarks, pull requests, separate AI reviews, and production releases for Mizuki’s capability updates.',
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
            Evidence-backed production changes{' '}
            {result.status !== 'error' && result.demo && <DemoNotice />}
          </p>
          <h1>Every capability claim is tied to evidence.</h1>
        </div>
        <div className="page-hero-aside">
          <p>
            A capability is shown as active only when its benchmark, code change, separate AI
            review, and production deployment are recorded. Missing or degraded evidence remains
            visible.
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
          <DataError title="Capability records are temporarily unavailable" />
        ) : result.status === 'empty' ? (
          <EmptyState title="No capability updates have been published yet">
            The first approved production update will appear here with its supporting records.
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
                              <strong>{capability.benchmarkAfter ?? 'Not measured yet'}</strong>
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
                              View change proposal <span aria-hidden="true">↗</span>
                            </a>
                          )}
                        </div>
                      </article>
                    ))}
                    {items.length === 0 && (
                      <p className="column-empty">No capabilities currently have this status.</p>
                    )}
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
