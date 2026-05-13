"use client";

import { api, type ContentBlock } from "@/lib/api";
import { shortHash, truncate } from "@/lib/format";
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
    .map((b) => (b.type === "text" ? b.text : `<data>`))
    .join(" ")
    .trim();
  return truncate(text || "(no reply yet)", 220);
}

function statusWord(status: string): string {
  switch (status) {
    case "ok":
      return "Done";
    case "error":
      return "Failed";
    case "pending":
      return "Pending";
    case "running":
      return "Running";
    default:
      return status[0].toUpperCase() + status.slice(1);
  }
}

export default function MessagesPage() {
  const { data, error, lastSyncMs } = usePoll(loadQueues, 3000);
  const tasks = data?.tasks ?? [];
  const results = data?.results ?? [];

  return (
    <>
      <PageHeader
        eyebrow="agents talking to each other"
        title="Messages"
        subhead="Tasks one agent has handed off to another, and the replies coming back. Every message needs a signed permission from you."
        syncMs={lastSyncMs}
        error={error}
      />

      <section className="split-2">
        <div className="panel">
          <div className="panel-head">
            <div>
              <p className="eyebrow">in flight</p>
              <h2>
                Outgoing tasks <span className="count">{tasks.length}</span>
              </h2>
            </div>
          </div>
          {tasks.length === 0 ? (
            <p className="empty">No tasks in flight between your agents.</p>
          ) : (
            <div className="records">
              {tasks.map((t) => (
                <article key={t.id} className="record fade-up">
                  <div className="ts">
                    <span>{shortHash(t.id, 10)}</span>
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
              <p className="eyebrow">replies</p>
              <h2>
                Incoming replies <span className="count">{results.length}</span>
              </h2>
            </div>
          </div>
          {results.length === 0 ? (
            <p className="empty">No replies yet.</p>
          ) : (
            <div className="records">
              {results.map((r) => (
                <article
                  key={r.task_id}
                  className={`record fade-up ${r.status !== "ok" ? "tone-warn" : ""}`}
                >
                  <div className="ts">
                    <span>{statusWord(r.status)}</span>
                  </div>
                  <div className="body">
                    <strong>Reply to task {shortHash(r.task_id, 10)}</strong>
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
