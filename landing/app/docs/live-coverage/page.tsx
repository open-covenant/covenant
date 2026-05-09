import Link from "next/link";

export const metadata = {
  title: "Live coverage",
  description:
    "Opt-in live test coverage across daemon, CLI, A2A, MCP, runtime, and model boundaries.",
};

const SURFACES = [
  ["Daemon IPC core", "covered", "daemon and CLI intent dispatch"],
  ["HTTP gateway", "mock only", "real daemon gateway smoke test"],
  ["CLI capability lifecycle", "covered", "capability revoke"],
  ["CLI audit feed", "covered", "audit purge after retention policy"],
  ["Peer authentication", "covered", "operator self-revoke rejection"],
  ["Peer listing", "covered", "ambiguous-prefix listing"],
  ["A2A mailbox", "covered", "explicit requeue repair"],
  ["MCP subprocess", "covered", "third-party fixture"],
  ["Runtime subprocess", "covered", "malformed stdout failure path"],
  ["Budget enforcement", "covered", "budget resume"],
  ["Local model", "external service", "model availability probes"],
];

export default function LiveCoveragePage() {
  return (
    <>
      <h1>Live coverage</h1>
      <p>
        Covenant keeps default CI deterministic while tracking which surfaces
        have opt-in live coverage. Live tests are Rust tests named{" "}
        <code>live_*</code> and marked with <code>#[ignore]</code>.
      </p>

      <h2>Commands</h2>
      <code>{`node agent-os/scripts/validate-live-coverage.mjs
bash agent-os/scripts/test-stats.sh
cd agent-os
cargo test --workspace --exclude covenant-settlement-program -- --ignored live_`}</code>

      <h2>Matrix</h2>
      <table>
        <thead>
          <tr>
            <th>Surface</th>
            <th>Status</th>
            <th>Next gap</th>
          </tr>
        </thead>
        <tbody>
          {SURFACES.map(([surface, status, gap]) => (
            <tr key={surface}>
              <td>{surface}</td>
              <td>{status}</td>
              <td>{gap}</td>
            </tr>
          ))}
        </tbody>
      </table>

      <h2>Related</h2>
      <ul>
        <li>
          <Link href="/architecture">System architecture</Link> — the
          surfaces under test.
        </li>
        <li>
          <Link href="/security">Security model</Link> — why real boundary
          tests matter.
        </li>
        <li>
          <Link href="/provenance">Provenance</Link> — evidence attached to
          committed autonomous work.
        </li>
      </ul>
    </>
  );
}
