# Linux gVisor Live Runner

This guide makes the opt-in `runsc` validation path reproducible for contributors and future CI hosts. It documents the current Linux gVisor contract without claiming that Covenant has production sandbox-grade execution by default.

## What This Validates

The live runner proves that `covenant-runtime` can dispatch an agent through the real `GvisorRunner` path:

- build an OCI bundle for a sandbox-required agent;
- invoke `runsc run --bundle <bundle> <id>`;
- mount the agent package read-only at `/workspace`;
- run with a read-only root filesystem;
- disable host-network access through a network namespace;
- pass the intent over stdin and parse the agent result from stdout;
- fail instead of falling back to trusted-local execution;
- clean the temporary bundle directory after completion.

It does not validate every future sandbox policy. The initial runner only accepts:

| Manifest field | Supported value |
| --- | --- |
| `[sandbox].backend` | `linux-gvisor` |
| `[sandbox].filesystem` | `read-only-package` |
| `[resources].network` | `off` |

Policies such as `ephemeral`, `host`, `outbound-https-only`, and `full` still fail closed until they have real enforcement.

## Host Requirements

Run this on a Linux host. macOS remains trusted-local only.

Required:

- Rust stable;
- `runsc` installed and executable by the test user;
- a root filesystem directory containing `/bin/sh`;
- permission for `runsc` to create the required namespaces on the host.

The runner expects `runsc` to be callable directly by the test process. Do not run the test under `sudo` as a workaround; fix host runtime permissions instead so the same invocation can be reused by CI.

Verify the runtime first:

```bash
runsc --version
```

If `runsc` is not on `PATH`, set `COVENANT_LIVE_RUNSC` to an absolute path.

## Rootfs

The live test only needs a minimal rootfs with `/bin/sh`. The rootfs is mounted read-only and the test agent package is mounted separately at `/workspace`.

Use an existing rootfs if your runner already provisions one:

```bash
export COVENANT_LIVE_GVISOR_ROOTFS=/opt/covenant/rootfs
test -x "$COVENANT_LIVE_GVISOR_ROOTFS/bin/sh"
```

For a local smoke test on a Linux machine with Docker available, export a small Alpine rootfs into a repository-local scratch directory:

```bash
mkdir -p .covenant-live/rootfs
image="${COVENANT_LIVE_ROOTFS_IMAGE:-alpine:3.20}"
cid="$(docker create "$image")"
docker export "$cid" | tar -C .covenant-live/rootfs -xf -
docker rm "$cid"

export COVENANT_LIVE_GVISOR_ROOTFS="$PWD/.covenant-live/rootfs"
test -x "$COVENANT_LIVE_GVISOR_ROOTFS/bin/sh"
```

Use a rootfs that matches the runner architecture. For CI, pin the rootfs source by digest or build artifact instead of relying on a moving image tag.

## Run the Live Test

From the repository root:

```bash
cd agent-os

export COVENANT_LIVE_GVISOR_ROOTFS="$PWD/../.covenant-live/rootfs"
export COVENANT_LIVE_RUNSC="${COVENANT_LIVE_RUNSC:-runsc}"

"$COVENANT_LIVE_RUNSC" --version
test -x "$COVENANT_LIVE_GVISOR_ROOTFS/bin/sh"

cargo test -p covenant-runtime --test live_gvisor -- --ignored live_gvisor_runner_dispatches_with_runsc
```

When `COVENANT_LIVE_GVISOR_ROOTFS` is unset, the ignored test takes a prerequisite-skip path and exits successfully. When the rootfs is set, missing `runsc`, an invalid rootfs, sandbox startup failure, or fallback execution is a real failure.

## Daemon Configuration

The daemon still defaults to trusted-local execution:

```bash
COVENANT_RUNTIME_BACKEND=trusted-local
```

To start the daemon with the initial Linux gVisor runtime:

```bash
COVENANT_RUNTIME_BACKEND=linux-gvisor
COVENANT_GVISOR_ROOTFS=/opt/covenant/rootfs
COVENANT_RUNSC=runsc
COVENANT_GVISOR_SCRATCH=$COVENANT_HOME/runtime/gvisor
```

`COVENANT_GVISOR_ROOTFS` is required when `COVENANT_RUNTIME_BACKEND=linux-gvisor`. Startup fails if the backend is unknown or the rootfs is missing.

## CI Adoption Criteria

Check host readiness before promoting the live test into CI:

```bash
node agent-os/scripts/gvisor-host-readiness.mjs --json
node agent-os/scripts/validate-gvisor-host-readiness.mjs
```

The readiness report is non-mutating. It reports the current host prerequisites separately from the project decisions needed for required CI.

Before this becomes a required CI job, the runner needs:

- a dedicated Linux runner image or setup step with `runsc` preinstalled;
- a pinned rootfs artifact that includes `/bin/sh`;
- a documented kernel/runtime compatibility baseline;
- captured `runsc --version` and rootfs provenance in CI logs;
- no dependence on operator home directories, SSH keys, shell profiles, or credential stores;
- a failure policy that blocks only sandbox-runtime changes until the runner is stable.

Until then, the matrix classifies Linux gVisor dispatch as external-service live coverage.

## Failure Triage

| Symptom | Meaning |
| --- | --- |
| `skip: requires Linux` | The host cannot run this validation path. Use Linux. |
| `skip: set COVENANT_LIVE_GVISOR_ROOTFS...` | The test was invoked without a rootfs; this is a prerequisite skip. |
| `runsc is not executable` | Install `runsc` or set `COVENANT_LIVE_RUNSC`. |
| `rootfs must contain /bin/sh` | The rootfs is not suitable for the current live test. |
| `NonZeroExit` from the runner | `runsc` started the sandbox but the agent failed. Inspect redacted stderr and host runtime logs. |

The expected invariant is strict: a sandbox-required agent must fail closed when `linux-gvisor` cannot satisfy the manifest. It must not downgrade to trusted-local subprocess execution.
