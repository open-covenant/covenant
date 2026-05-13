"use client";

import Link from "next/link";
import { api } from "@/lib/api";
import { formatTimestamp, shortHash, truncate } from "@/lib/format";
import { usePoll } from "@/lib/usePoll";
import { PageHeader } from "../components/PageHeader";

async function loadIntents() {
  return api.recentAudit(100);
}

export default function TasksPage() {
  const { data, error, lastSyncMs } = usePoll(loadIntents, 3000);
  const events = data?.events ?? [];
  const tasks = events
    .filter((e) => e.kind.type === "intent_dispatched")
    .slice()
    .reverse();

  return (
    <>
      <PageHeader
        eyebrow="what you've asked for"
        title="Tasks"
        subhead="Every task you've sent. Click one to see its signed steps."
        syncMs={lastSyncMs}
        error={error}
      />

      <section className="panel flush">
        <div className="panel-head" style={{ padding: "22px 22px 0" }}>
          <div>
            <p className="eyebrow">recent</p>
            <h2>
              Recent tasks <span className="count">{tasks.length}</span>
            </h2>
          </div>
        </div>

        {tasks.length === 0 ? (
          <p className="empty" style={{ padding: "30px 22px" }}>
            Nothing here yet. Tasks you send will show up in this list.
          </p>
        ) : (
          <div className="intent-list">
            {tasks.map((event) => {
              if (event.kind.type !== "intent_dispatched") return null;
              const tone = event.kind.matched_agent ? "ok" : "warn";
              return (
                <Link
                  key={event.id}
                  href={`/intents/${event.kind.intent_id}`}
                  className={`intent-row tone-${tone}`}
                >
                  <div className="left">
                    <span className="text-muted text-mono ts">
                      {formatTimestamp(event.timestamp_ms)}
                    </span>
                    <p className="text">{truncate(event.kind.intent_text, 180)}</p>
                  </div>
                  <div className="right">
                    <span className="agent">
                      {event.kind.matched_agent ? (
                        <>
                          Ran by <strong>{event.kind.matched_agent}</strong>
                        </>
                      ) : (
                        <em>no agent matched</em>
                      )}
                    </span>
                    <span className="hash">{shortHash(event.kind.result_hash_hex, 16)}</span>
                  </div>
                </Link>
              );
            })}
          </div>
        )}
      </section>

    </>
  );
}
