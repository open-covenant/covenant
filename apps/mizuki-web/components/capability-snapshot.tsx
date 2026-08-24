import Link from 'next/link';
import type { Capability } from '@/lib/types';
import { StatusBadge } from './status-badge';

export function CapabilitySnapshot({ capabilities }: { capabilities: Capability[] }) {
  return (
    <div className="capability-snapshot">
      {capabilities.slice(0, 3).map((capability) => (
        <article key={capability.id}>
          <div className="capability-index" aria-hidden="true">
            {String(capabilities.indexOf(capability) + 1).padStart(2, '0')}
          </div>
          <div>
            <div className="capability-title-row">
              <h3>{capability.name}</h3>
              <StatusBadge state={capability.state} />
            </div>
            <p>{capability.description}</p>
            {capability.benchmarkAfter !== undefined && (
              <div className="benchmark-delta">
                <span>{capability.benchmarkBefore ?? '—'}</span>
                <span aria-hidden="true">→</span>
                <strong>{capability.benchmarkAfter}</strong>
                <small>{capability.benchmarkUnit}</small>
              </div>
            )}
          </div>
        </article>
      ))}
      <Link href="/capabilities" className="text-link">
        View all capability records <span aria-hidden="true">↗</span>
      </Link>
    </div>
  );
}
