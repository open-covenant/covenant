import Link from "next/link";

export const metadata = {
  title: "Live coverage",
  description:
    "Opt-in live test coverage across daemon, CLI, A2A, MCP, runtime, and model boundaries.",
};

const SURFACES = [
  ["Daemon IPC core", "covered", "daemon IPC plus CLI intent/version"],
  ["State verifier", "covered", "live drift fixture"],
  ["HTTP gateway", "covered", "version, auth, and tools-call round trip"],
  ["CLI capability lifecycle", "covered", "capability purge after retention defaults"],
  ["CLI audit feed", "covered", "scoped audit purge rejection"],
  ["Peer authentication", "covered", "forced self-revoke recovery fixture"],
  ["Peer listing", "covered", "ambiguous-prefix listing"],
  ["A2A mailbox", "covered", "stale-lease guard failure"],
  ["MCP subprocess", "covered", "third-party fixture"],
  ["Runtime subprocess", "covered", "daemon dispatch failure receipts"],
  ["Linux gVisor runtime", "external service", "documented Linux runsc runner"],
  ["Budget enforcement", "covered", "budget resume"],
  ["Settlement receipts", "covered", "scoped receipt filters"],
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
cargo test --workspace --exclude covenant-settlement-program -- --ignored live_

# Before targeted live CLI tests:
cargo build -p covenant --locked
cargo test -p covenantd --test live_cli_version -- --ignored live_cli_version_reads_protocol_info_without_token

# Linux gVisor runtime validation:
COVENANT_LIVE_GVISOR_ROOTFS=/path/to/rootfs \\
  cargo test -p covenant-runtime --test live_gvisor -- --ignored live_gvisor_runner_dispatches_with_runsc`}</code>

      <p>
        Linux gVisor coverage requires a Linux host with <code>runsc</code> and
        a rootfs containing <code>/bin/sh</code>. The repeatable setup lives in{" "}
        <Link href="/gvisor-live-runner">Linux gVisor runner</Link>.
      </p>

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
          <Link href="/gvisor-live-runner">Linux gVisor runner</Link> — host
          setup for the sandbox live path.
        </li>
        <li>
          <Link href="/provenance">Provenance</Link> — evidence attached to
          committed autonomous work.
        </li>
      </ul>
    </>
  );
}
