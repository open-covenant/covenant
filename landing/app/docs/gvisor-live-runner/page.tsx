import Link from "next/link";
import { buildDocsMetadata } from "../_meta";

export const metadata = buildDocsMetadata("gvisor-live-runner", "Linux gVisor live runner", "Repeatable Linux host setup for Covenant's opt-in runsc live sandbox validation path.");

export default function GvisorLiveRunnerPage() {
  return (
    <>
      <h1>Linux gVisor live runner</h1>
      <p>
        Covenant has an initial Linux gVisor runtime runner, but production
        sandbox claims require reproducible live validation. This guide defines
        the host contract for the opt-in <code>runsc</code> test path.
      </p>

      <h2>What it validates</h2>
      <ul>
        <li>OCI bundle generation for a sandbox-required agent.</li>
        <li>
          Real <code>runsc run --bundle</code> dispatch through the runtime
          crate.
        </li>
        <li>
          Read-only package mount at <code>/workspace</code> and read-only
          root filesystem.
        </li>
        <li>Network namespace isolation for the current network-off policy.</li>
        <li>No fallback to trusted-local execution when sandbox startup fails.</li>
        <li>Cleanup of the temporary bundle directory after execution.</li>
      </ul>

      <h2>Supported manifest subset</h2>
      <table>
        <thead>
          <tr>
            <th>Field</th>
            <th>Supported value</th>
          </tr>
        </thead>
        <tbody>
          <tr>
            <td>
              <code>[sandbox].backend</code>
            </td>
            <td>
              <code>linux-gvisor</code>
            </td>
          </tr>
          <tr>
            <td>
              <code>[sandbox].filesystem</code>
            </td>
            <td>
              <code>read-only-package</code>
            </td>
          </tr>
          <tr>
            <td>
              <code>[resources].network</code>
            </td>
            <td>
              <code>off</code>
            </td>
          </tr>
        </tbody>
      </table>

      <p>
        Other sandbox policies still fail closed until their enforcement exists
        in code.
      </p>

      <h2>Host requirements</h2>
      <ul>
        <li>Linux host.</li>
        <li>Rust stable.</li>
        <li>
          <code>runsc</code> installed and executable by the test user.
        </li>
        <li>
          Root filesystem directory containing <code>/bin/sh</code>.
        </li>
        <li>
          Host permissions that allow <code>runsc</code> to create the required
          namespaces.
        </li>
      </ul>

      <h2>Rootfs smoke setup</h2>
      <code>{`mkdir -p .covenant-live/rootfs
image="\${COVENANT_LIVE_ROOTFS_IMAGE:-alpine:3.20}"
cid="$(docker create "$image")"
docker export "$cid" | tar -C .covenant-live/rootfs -xf -
docker rm "$cid"

export COVENANT_LIVE_GVISOR_ROOTFS="$PWD/.covenant-live/rootfs"
test -x "$COVENANT_LIVE_GVISOR_ROOTFS/bin/sh"`}</code>

      <h2>Run the test</h2>
      <code>{`cd agent-os

export COVENANT_LIVE_GVISOR_ROOTFS="$PWD/../.covenant-live/rootfs"
export COVENANT_LIVE_RUNSC="\${COVENANT_LIVE_RUNSC:-runsc}"

"$COVENANT_LIVE_RUNSC" --version
test -x "$COVENANT_LIVE_GVISOR_ROOTFS/bin/sh"

cargo test -p covenant-runtime --test live_gvisor -- --ignored live_gvisor_runner_dispatches_with_runsc`}</code>

      <h2>CI adoption criteria</h2>
      <ul>
        <li>Dedicated Linux runner image or setup step with <code>runsc</code>.</li>
        <li>Pinned rootfs artifact that includes <code>/bin/sh</code>.</li>
        <li>Captured <code>runsc --version</code> and rootfs provenance.</li>
        <li>No dependence on operator home directories or credential stores.</li>
        <li>
          Failure policy scoped to sandbox-runtime changes until the runner is
          stable.
        </li>
      </ul>

      <h2>Related</h2>
      <ul>
        <li>
          <Link href="/security">Security model</Link> — current trust
          boundaries and unsupported claims.
        </li>
        <li>
          <Link href="/live-coverage">Live coverage</Link> — real-boundary test
          inventory.
        </li>
        <li>
          <a
            href="https://github.com/open-covenant/covenant/blob/main/docs/gvisor-live-runner.md"
            target="_blank"
            rel="noopener noreferrer"
          >
            Repository guide
          </a>{" "}
          — source-of-truth runner details.
        </li>
      </ul>
    </>
  );
}
