import Link from "next/link";
import { buildDocsMetadata } from "../_meta";

export const metadata = buildDocsMetadata("autonomy", "Autonomous workflow", "Task lifecycle, validation gates, continuation, and handoff summaries for Covenant's autonomous development loop.");

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
      <p>
        The workflow is driven by a machine-readable lifecycle definition,
        scoped task records with explicit gates and verification, an
        append-only transition log, and a durable seed queue that refills
        when no active task is ready. The concrete files and runner
        scripts live in engineering-loop tooling rather than the public
        scripts directory; the recursive engineering model in{" "}
        <a
          href="https://github.com/open-covenant/covenant/blob/main/BUILT.md"
          target="_blank"
          rel="noopener noreferrer"
        >
          BUILT.md
        </a>{" "}
        describes the contract that those tools implement.
      </p>

      <h2>Summary contract</h2>
      <p>
        Sprint and handoff summaries are derived from tracked task state and
        the transition log. They report scoped task counts, state counts,
        active work, blocked work, recently integrated tasks, and recent
        transition events — deterministic outputs that a fresh session can
        consume without private context.
      </p>

      <h2>Validation</h2>
      <ul>
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
            href="https://github.com/open-covenant/covenant/blob/main/BUILT.md"
            target="_blank"
            rel="noopener noreferrer"
          >
            Recursive engineering model
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
