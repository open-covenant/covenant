"use client";

import { api, type ContentBlock } from "@/lib/api";
import { short, truncate } from "@/lib/audit";
import { usePoll } from "@/lib/usePoll";
import { PageHeader } from "../components/PageHeader";

async function loadQueues() {
  const [tasks, results] = await Promise.all([
    api.recentA2ATasks(40),
    api.recentA2AResults(40),
  ]);
  return { tasks: tasks.tasks, results: results.results };
}

function summary(content: ContentBlock[]): string {
  const text = content
    .map((b) => (b.type === "text" ? b.text : `<json:${JSON.stringify(b.value).slice(0, 80)}…>`))
    .join(" ")
    .trim();
  return truncate(text || "(empty)", 220);
}

export default function QueuesPage() {
  const { data, error, lastSyncMs } = usePoll(loadQueues, 3000);
  const tasks = data?.tasks ?? [];
  const results = data?.results ?? [];

  return (
    <>
      <PageHeader
        eyebrow="agent-to-agent"
        title="Queues"
        subhead="Tasks in flight between agents, and results waiting to be claimed. Every message is gated by a signed capability."
        syncMs={lastSyncMs}
        error={error}
      />

      <section className="split-2">
        <div className="panel">
          <div className="panel-head">
            <div>
              <p className="eyebrow">tasks</p>
              <h2>
                Queued tasks <span className="count">{tasks.length}</span>
              </h2>
            </div>
          </div>
          {tasks.length === 0 ? (
            <p className="empty">No tasks in flight.</p>
          ) : (
            <div className="records">
              {tasks.map((t) => (
                <article key={t.id} className="record fade-up">
                  <div className="ts">
                    <span>{short(t.id, 10)}</span>
                  </div>
                  <div className="body">
                    <strong>
                      {t.sender.display} → {t.recipient.display}
                    </strong>
                    <p>{truncate(t.intent_text, 200)}</p>
                  </div>
                </article>
              ))}
            </div>
          )}
        </div>

        <div className="panel">
          <div className="panel-head">
            <div>
              <p className="eyebrow">results</p>
              <h2>
                Results pending <span className="count">{results.length}</span>
              </h2>
            </div>
          </div>
          {results.length === 0 ? (
            <p className="empty">No results pending.</p>
          ) : (
            <div className="records">
              {results.map((r) => (
                <article key={r.task_id} className={`record fade-up ${r.status !== "ok" ? "tone-warn" : ""}`}>
                  <div className="ts">
                    <span>{r.status}</span>
                  </div>
                  <div className="body">
                    <strong>task {short(r.task_id, 10)}</strong>
                    <p>
                      {summary(r.content)}
                      {r.error_message && ` · error: ${r.error_message}`}
                    </p>
                  </div>
                </article>
              ))}
            </div>
          )}
        </div>
      </section>
    </>
  );
}
