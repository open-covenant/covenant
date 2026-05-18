import Link from "next/link";
import { buildDocsMetadata } from "../_meta";

export const metadata = buildDocsMetadata("validation", "Validation profile", 'The Covenant validation profile: operating surfaces, evidence requirements, and live boundary checks.');

const SURFACES = [
  "Rust workspace with daemon, CLI, TUI, IPC, HTTP, runtime, router, memory, permissions, audit, identity, peer-auth, MCP, A2A, LLM, tools, budget, and local settlement crates.",
  "Local workflows for intent dispatch, capabilities, peers, memory, audit, A2A, tools, receipts, chain status, and verification.",
  "Trusted-local subprocess execution, fail-closed sandbox-required manifests, and opt-in Linux gVisor validation where host prerequisites are met.",
  "Autonomy task records, transition events, project memory, live coverage matrix, identity guards, and commit-scoped provenance envelopes.",
];

export default function ValidationProfilePage() {
  return (
    <>
      <h1>Validation profile</h1>
      <p>
        Covenant is validated as local-first infrastructure for autonomous
        software engineering systems. The profile below describes the operating
        surfaces, evidence model, and live boundary checks used to keep the
        daemon, CLI, policy, memory, audit, and provenance layers accountable.
      </p>

      <h2>Operating surfaces</h2>
      <ul>
        {SURFACES.map((item) => (
          <li key={item}>{item}</li>
        ))}
      </ul>

      <h2>Evidence</h2>
      <p>
        Validation records the commit, supported host assumptions, command
        results, capability status, live coverage, runtime security boundary,
        provenance envelope, and any audit-root attestation generated for the
        candidate.
      </p>

      <p>
        Alpha candidates use the read-only release evidence helper to capture
        the commit, branch, dirty-file count, recommended gates, and non-claims.
        Evidence is accepted only after the listed gates are run and recorded in
        a release bundle.
      </p>

      <code>{`bash agent-os/scripts/validate.sh --quick
node agent-os/scripts/provenance.mjs verify-all
pnpm --dir landing build
git diff --check`}</code>

      <h2>Related</h2>
      <ul>
        <li>
          <Link href="/alpha-release">Alpha release contract</Link> — source
          alpha boundary and release evidence bundle.
        </li>
        <li>
          <Link href="/security">Security model</Link> — current trust
          boundary.
        </li>
        <li>
          <Link href="/live-coverage">Live coverage</Link> — real-boundary
          validation matrix.
        </li>
        <li>
          <Link href="/provenance">Provenance</Link> — release evidence path.
        </li>
      </ul>
    </>
  );
}
