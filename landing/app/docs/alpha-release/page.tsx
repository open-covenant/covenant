import Link from "next/link";

export const metadata = {
  title: "Alpha release",
  description:
    "The Covenant alpha release contract: supported source-built scope, release blockers, explicit non-claims, and post-alpha research.",
};

const INCLUDED = [
  "Rust workspace with daemon, CLI, IPC, HTTP, runtime, memory, permissions, audit, identity, peer-auth, MCP, A2A, budget, and local settlement crates.",
  "Source-built local workflows for intent dispatch, capabilities, peers, memory, audit, A2A, tools, receipts, chain status, and verification.",
  "Trusted-local subprocess execution, fail-closed sandbox-required manifests, and opt-in Linux gVisor validation where host prerequisites are met.",
  "Autonomy task records, transition events, project memory, live coverage matrix, identity guards, and commit-scoped provenance envelopes.",
];

const NON_CLAIMS = [
  "production sandbox-grade execution by default",
  "on-chain settlement on mainnet",
  "deployed and audited settlement program",
  "public release signing key custody",
  "transparency-log publication",
  "package-manager installers",
  "stable SDKs",
  "marketplace or registry operation",
  "multi-host production readiness",
  "safe execution for untrusted third-party agents",
];

export default function AlphaReleasePage() {
  return (
    <>
      <h1>Alpha release contract</h1>
      <p>
        The alpha boundary is source-built, local-first infrastructure for
        engineers and researchers. It is not an installer-backed consumer
        product, hosted service, production sandbox, or live settlement network.
      </p>

      <p>
        Human approval is required before any alpha tag, release artifact,
        package, or announcement is created or published.
      </p>

      <h2>Supported alpha scope</h2>
      <ul>
        {INCLUDED.map((item) => (
          <li key={item}>{item}</li>
        ))}
      </ul>

      <h2>Required evidence</h2>
      <p>
        A release candidate must record the release commit, tag candidate,
        supported host assumptions, validation results, capability status,
        live coverage, runtime security boundary, provenance envelope, and any
        audit-root attestation generated for the candidate.
      </p>

      <code>{`bash agent-os/scripts/validate.sh --quick
node agent-os/scripts/validate-autonomy.mjs
node agent-os/scripts/validate-live-coverage.mjs
node agent-os/scripts/validate-git-identity.mjs
node agent-os/scripts/provenance.mjs verify-all
pnpm --dir landing build
git diff --check`}</code>

      <h2>Explicit non-claims</h2>
      <ul>
        {NON_CLAIMS.map((item) => (
          <li key={item}>{item}</li>
        ))}
      </ul>

      <h2>Release blockers</h2>
      <p>
        Failed local build, failed quick validation, invalid autonomy records,
        unsupported public claims, failed Git identity guard, personal or
        machine-local metadata in the release commit, stale security docs, or
        missing human release approval block an alpha tag.
      </p>

      <h2>Related</h2>
      <ul>
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
