# Runtime Sandbox Security Contract

This document defines the security contract for Covenant runtime isolation. It separates trusted-local execution, the initial Linux gVisor runtime runner, and the remaining production work so public claims stay aligned with the code.

## Current State

Implemented:

- Agent manifests may declare `[sandbox]`.
- `sandbox.required = true` is rejected unless the manifest names a sandbox-grade backend.
- The current subprocess runner is `trusted-local`; it refuses to run agents that require sandbox-grade isolation.
- `GvisorRunner` prepares a restrictive OCI bundle and invokes `runsc`.
- The initial `GvisorRunner` supports `filesystem = "read-only-package"` and `network = "off"` only.
- `covenantd` can select `trusted-local` or `linux-gvisor` at startup through explicit runtime backend configuration.
- The live coverage matrix includes an ignored Linux gVisor dispatch test gated on `runsc` and an explicit rootfs.
- A repeatable Linux runner guide documents the host, `runsc`, rootfs, and CI adoption requirements for that live path.
- Sandbox stderr redacts configured host-local paths before surfacing failure text.

Not implemented:

- `outbound-https-only`, `full`, `ephemeral`, and `host` sandbox policies are not enforced by the initial runner; they fail closed instead.
- macOS execution is trusted-local only.
- Default CI does not yet provision the Linux host, `runsc`, or rootfs needed for live sandbox validation.

## Trust Boundary

The trusted-local runner is useful for first-party automation and local development. It is not a security boundary against hostile agent code.

Trusted-local protects:

- Covenant-mediated state mutations through daemon-side capability checks.
- Runtime wall-clock budget by killing long-running subprocesses.
- Agent protocol attribution because the daemon chooses which manifest produced a result.

Trusted-local does not protect:

- Host filesystem reads available to the operator user.
- Host environment variables inherited by a child process.
- Network access beyond whatever the host OS allows.
- Memory, CPU, syscall, or device abuse beyond the current timeout guard.
- Malicious code executed as the same user outside the daemon protocol.

## Sandbox-Required Semantics

If an agent declares:

```toml
[sandbox]
required = true
backend = "linux-gvisor"
```

then Covenant must fail closed when the active runtime cannot satisfy that backend. It must not silently downgrade to trusted-local subprocess execution.

This invariant is enforced by `covenant-manifest` validation, `covenant-runtime` subprocess dispatch, and daemon startup backend selection. If the daemon runs with the default `trusted-local` backend, sandbox-required agents fail rather than downgrading.

## Daemon Backend Configuration

`covenantd` defaults to trusted-local execution:

```bash
COVENANT_RUNTIME_BACKEND=trusted-local
```

To opt into the initial Linux gVisor runner:

```bash
COVENANT_RUNTIME_BACKEND=linux-gvisor
COVENANT_GVISOR_ROOTFS=/path/to/rootfs
```

Optional gVisor settings:

```bash
COVENANT_RUNSC=runsc
COVENANT_GVISOR_SCRATCH=$COVENANT_HOME/runtime/gvisor
```

`COVENANT_GVISOR_ROOTFS` is required for `linux-gvisor`. Missing or unknown backend configuration fails daemon startup. Runtime execution errors from `runsc` are surfaced as dispatch failures; Covenant must not fall back to trusted-local execution for sandbox-required agents.

## Live gVisor Validation

The runtime crate has an opt-in live test for the real `runsc` dispatch path. It is intentionally ignored by default because the host requirements are not portable. The repeatable setup is documented in [Linux gVisor Live Runner](gvisor-live-runner.md).

```bash
cd agent-os
COVENANT_LIVE_GVISOR_ROOTFS=/path/to/rootfs \
  cargo test -p covenant-runtime --test live_gvisor -- --ignored live_gvisor_runner_dispatches_with_runsc
```

Optional:

```bash
COVENANT_LIVE_RUNSC=/path/to/runsc
```

The rootfs must contain `/bin/sh`. When the rootfs is not provided, the test takes a prerequisite-skip path and exits successfully. When the rootfs is provided, missing `runsc`, invalid rootfs layout, sandbox startup failure, or fallback behavior is a test failure.

## First Production Backend

The accepted first production sandbox backend is Linux gVisor through `runsc`. The runtime crate contains the first runner boundary, the daemon can select it explicitly, and an opt-in live dispatch test exists. Covenant still cannot claim production sandbox-grade local execution until Linux host requirements and repeatable CI coverage are complete. The backend must meet this minimum contract:

- prepare an OCI bundle from the agent package without mounting the host home directory;
- mount the agent package read-only unless the manifest explicitly requests an ephemeral writable layer;
- start with no ambient host secrets or arbitrary inherited environment variables;
- enforce manifest-visible network policy;
- enforce wall-clock timeout and kill the sandbox on timeout;
- surface sandbox setup failure as dispatch failure, not fallback execution;
- redact host-local paths and secret-looking values from operator-facing logs;
- provide live Linux-only validation gated on `runsc` availability.

## Filesystem Policy

Manifest values are parsed now. The initial gVisor runner enforces `read-only-package` with `network = "off"` and rejects other policies until they have real enforcement:

| Policy | Meaning |
| --- | --- |
| `read-only-package` | Agent package is visible read-only. This is the default. |
| `ephemeral` | Planned. Agent receives a writable scratch layer that is discarded after dispatch. |
| `host` | Planned privileged policy. This must require explicit operator approval before use by untrusted agents. |

Policies other than the initial enforced subset must not be described as available sandbox behavior.

## Security Review Checklist

Runtime sandbox changes require review against this checklist:

- Does any code path downgrade `sandbox.required = true` to trusted-local execution?
- Does the backend mount `$HOME`, the repo root, SSH keys, credential stores, or shell config by default?
- Does the backend inherit all operator environment variables?
- Does network access match the manifest policy?
- Are timeout and cleanup enforced when agent startup hangs?
- Are sandbox setup errors auditable without leaking host-local paths or secrets?
- Do public docs still distinguish trusted-local, implemented sandbox behavior, and planned behavior?
- Is there at least one failure-mode test for required-sandbox refusal?

## Human Escalation

Humans retain authority over:

- approving any public claim that Covenant safely executes hostile third-party agents;
- choosing production Linux host requirements for sandbox validation;
- enabling `host` filesystem policy for untrusted agents;
- publishing signed releases or transparency-log entries for sandboxed execution claims.
