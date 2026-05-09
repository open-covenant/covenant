import Link from "next/link";

export const metadata = {
  title: "Autonomous workflow",
  description:
    "Task lifecycle, validation gates, continuation, and handoff summaries for Covenant's autonomous development loop.",
};

export default function AutonomyPage() {
  return (
    <>
      <h1>Autonomous workflow</h1>
      <p>
        Covenant treats autonomous maintenance as an operating-layer surface.
        Work is represented as task records, transition events, validation
        evidence, and handoff summaries rather than private chat state.
      </p>

      <h2>Lifecycle</h2>
      <code>{`proposed -> triaged -> planned -> in_progress -> self_review -> validation -> ready -> integrated`}</code>

      <p>
        Security-sensitive or broad changes can add <code>cross_review</code>{" "}
        or move through <code>repair</code>. A task enters{" "}
        <code>blocked</code> only when a human-only input is actually required.
      </p>

      <h2>Control surface</h2>
      <ul>
        <li>
          <code>agent-os/autonomy/workflow.json</code> — states, roles, gates,
          transitions, and definition of done.
        </li>
        <li>
          <code>agent-os/autonomy/tasks/*.json</code> — scoped work records.
        </li>
        <li>
          <code>agent-os/autonomy/events.jsonl</code> — append-only transition
          history.
        </li>
        <li>
          <code>agent-os/autonomy/backlog.json</code> — durable seed queue used
          when no active task is ready.
        </li>
      </ul>

      <h2>Commands</h2>
      <code>{`node agent-os/scripts/autonomy-next.mjs
node agent-os/scripts/autonomy-continue.mjs
node agent-os/scripts/autonomy-seed-next.mjs --dry-run
node agent-os/scripts/autonomy-transition.mjs <task-id> <state> --actor <role> --note "<why>"
node agent-os/scripts/autonomy-summary.mjs --since 2026-05-09
node agent-os/scripts/autonomy-summary.mjs --format json
node agent-os/scripts/validate-autonomy.mjs`}</code>

      <h2>Summary contract</h2>
      <p>
        <code>autonomy-summary.mjs</code> emits Markdown for human handoffs and
        JSON for monitors. It reports scoped task counts, state counts, active
        work, blocked work, recently integrated tasks, and recent transition
        events directly from tracked repository state.
      </p>

      <h2>Validation</h2>
      <ul>
        <li>
          Task records and transition logs are checked by{" "}
          <code>validate-autonomy.mjs</code>.
        </li>
        <li>
          Git authors and committers are checked by{" "}
          <code>validate-git-identity.mjs</code>.
        </li>
        <li>
          Commit evidence is checked by <code>provenance.mjs verify-all</code>.
        </li>
        <li>
          Landing documentation must build with <code>pnpm --dir landing build</code>.
        </li>
      </ul>

      <h2>Related</h2>
      <ul>
        <li>
          <a
            href="https://github.com/open-covenant/covenant/blob/main/docs/autonomous-development.md"
            target="_blank"
            rel="noopener noreferrer"
          >
            Autonomous development protocol
          </a>
        </li>
        <li>
          <Link href="/provenance">Provenance</Link> — commit-scoped evidence.
        </li>
        <li>
          <Link href="/live-coverage">Live coverage</Link> — opt-in
          real-boundary tests.
        </li>
      </ul>
    </>
  );
}
