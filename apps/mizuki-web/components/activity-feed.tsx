'use client';

import { useEffect, useMemo, useState } from 'react';
import Link from 'next/link';
import { formatUsd, relativeTime, truncateAddress } from '@/lib/format';
import type { ActivityEvent } from '@/lib/types';

const maxEvents = 30;

export function ActivityFeed({
  initial,
  live = true,
  compact = false,
}: {
  initial: ActivityEvent[];
  live?: boolean;
  compact?: boolean;
}) {
  const [events, setEvents] = useState(initial);
  const [connection, setConnection] = useState<'connecting' | 'live' | 'offline'>(
    live ? 'connecting' : 'offline',
  );

  useEffect(() => {
    setEvents(initial);
  }, [initial]);

  useEffect(() => {
    if (!live) return;
    const source = new EventSource('/api/mizuki/v1/events');
    const receive = (event: MessageEvent<string>) => {
      try {
        const next = JSON.parse(event.data) as ActivityEvent;
        if (!next.id || !next.kind || !next.occurredAt) return;
        setEvents((current) =>
          [next, ...current.filter((item) => item.id !== next.id)].slice(0, maxEvents),
        );
      } catch {
        // Ignore malformed public events and keep the last valid feed.
      }
    };
    source.onopen = () => setConnection('live');
    source.onmessage = receive;
    source.addEventListener('activity', receive as EventListener);
    source.onerror = () => setConnection('offline');
    return () => source.close();
  }, [live]);

  const visible = useMemo(() => (compact ? events.slice(0, 5) : events), [compact, events]);

  return (
    <div className="activity-feed">
      <div className="feed-status" aria-live="polite">
        <span className={`feed-dot feed-${connection}`} aria-hidden="true" />
        {connection === 'live'
          ? 'Live stream'
          : connection === 'connecting'
            ? 'Connecting'
            : live
              ? 'Reconnecting'
              : 'Recorded activity'}
      </div>
      <ol>
        {visible.map((event) => (
          <li key={event.id}>
            <span className={`event-mark event-${event.kind}`} aria-hidden="true" />
            <div className="event-body">
              <div className="event-heading">
                <strong>{event.title}</strong>
                <time dateTime={event.occurredAt}>{relativeTime(event.occurredAt)}</time>
              </div>
              <p>{event.description}</p>
              <div className="event-receipts">
                {event.amountUsd !== undefined && <span>{formatUsd(event.amountUsd)}</span>}
                {event.transaction && <span>tx {truncateAddress(event.transaction, 4)}</span>}
                {event.href &&
                  (event.href.startsWith('/') ? (
                    <Link href={event.href}>Inspect ↗</Link>
                  ) : (
                    <a href={event.href} target="_blank" rel="noreferrer">
                      Inspect ↗
                    </a>
                  ))}
              </div>
            </div>
          </li>
        ))}
      </ol>
    </div>
  );
}
