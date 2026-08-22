'use client';

import { useMemo, useState } from 'react';
import type { Bounty, BountyState } from '@/lib/types';
import { BountyCard } from './bounty-card';

type Filter = 'all' | 'available' | 'in_progress' | 'completed';

const groups: Record<Exclude<Filter, 'all'>, BountyState[]> = {
  available: ['open', 'awaiting_funding'],
  in_progress: ['claimed', 'pr_submitted', 'validating', 'disputed'],
  completed: ['accepted', 'released', 'refunded'],
};

export function BountyBoard({ bounties }: { bounties: Bounty[] }) {
  const [filter, setFilter] = useState<Filter>('all');
  const visible = useMemo(
    () =>
      filter === 'all'
        ? bounties
        : bounties.filter((bounty) => groups[filter].includes(bounty.state)),
    [bounties, filter],
  );

  return (
    <div>
      <div className="board-controls">
        <div className="filter-group" aria-label="Filter bounties">
          {(['all', 'available', 'in_progress', 'completed'] as const).map((value) => (
            <button
              type="button"
              className={filter === value ? 'active' : ''}
              aria-pressed={filter === value}
              onClick={() => setFilter(value)}
              key={value}
            >
              {value.replaceAll('_', ' ')}
            </button>
          ))}
        </div>
        <span>
          {visible.length} public {visible.length === 1 ? 'bounty' : 'bounties'}
        </span>
      </div>
      {visible.length > 0 ? (
        <div className="bounty-grid board-grid">
          {visible.map((bounty) => (
            <BountyCard bounty={bounty} key={bounty.id} />
          ))}
        </div>
      ) : (
        <div className="data-state empty-state">
          <span className="data-state-mark" aria-hidden="true">
            0
          </span>
          <div>
            <strong>No bounties in this state</strong>
            <p>Change the filter to inspect the rest of the public record.</p>
          </div>
        </div>
      )}
    </div>
  );
}
