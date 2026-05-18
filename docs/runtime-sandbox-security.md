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
- Default CI does not run live sandbox validation on every PR — only sandbox-runtime path PRs trigger the dedicated `.github/workflows/gvisor-live.yml` workflow, which provisions `runsc` and the rootfs, and that workflow is not yet a required check.

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

The runtime crate has an opt-in live test for the real `runsc` dispatch path. It is intentionally ignored by default because the host requirements are not portable. Host prerequisites include a Linux kernel, a usable `runsc` binary on `PATH`, and a `COVENANT_LIVE_GVISOR_ROOTFS` directory that resolves to a minimal Alpine rootfs; the GitHub Actions workflow `.github/workflows/gvisor-live.yml` exercises the same path on `ubuntu-22.04` for sandbox-runtime changes.

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

## Budget-Driven Preempt (work in progress)

The current wall-clock guard kills a subprocess only after the agent-declared `cpu_ms_per_task` elapses. The hard budget-preempt path extends this so the runtime can also terminate a subprocess that is projected to exceed its remaining credit budget *before* it completes naturally. The path is being assembled across several sub-slices; this section lists what is shipped today and what is explicitly not yet wired so operators can plan around the partial state.

Shipped primitives:

- `covenant-runtime::SubprocessRunner` and `GvisorRunner` both spawn the agent (or the runsc host process, for gVisor) in a new POSIX process group via `process_group(0)` on Unix. A subsequent `kill(-pid, SIG)` targets every grandchild the agent forked, not just the immediate child. The configuration is a no-op on non-Unix; the documented gap remains.
- `covenant-runtime::SubprocessTracker` and `TrackedSubprocess` are the in-memory primitives the daemon uses to look up a running subprocess's pid by intent id. The daemon constructs the tracker once at startup and passes it into both runners; on every successful spawn the runner registers an entry keyed by `intent.id` and the entry is unregistered when the dispatch returns (success, error, or timeout-driven kill).
- `covenant-runtime::preempt_subprocess_pg(pid, grace)` is the pure async dispatcher function that sends `SIGTERM` to a process group, polls every 50ms up to `grace` for the group to drain via `kill(-pid, 0)==ESRCH`, then sends `SIGKILL` if any member is still alive. It returns a `PreemptOutcome` enum the daemon-side audit emitter will map to `BudgetPreempted` or `BudgetPreemptFailed` rows.
- `covenant-budget::project_overshoot(...)` is the pure projection function the daemon-side tick will call to decide whether an in-flight subprocess is on track to exceed its remaining budget. `BudgetProjectionPolicy::NoExtrapolation` preserves the existing post-completion-only behavior; `LinearExtrapolation` short-circuits below either an observation-window or sample-count threshold to avoid spurious early triggers.
- `covenant-audit::AuditKind::BudgetPreempted` and `BudgetPreemptFailed` are the audit rows the preempt path will emit on successful termination and on signal failure respectively. Both are wired into `audit_kind_requires_persistence` so the daemon refuses to drop them on audit-write failure.

Not yet wired:

- Daemon-side projection tick that walks the tracker, calls `project_overshoot`, and pushes flagged intents onto a bounded preempt queue.
- Daemon-side consumer that drains the preempt queue, calls `preempt_subprocess_pg` per intent_id, and emits the matching audit row from the returned `PreemptOutcome`.
- The grace window environment variable `COVENANT_BUDGET_PREEMPT_GRACE_MS` (planned default: `2000`). Operators should not depend on it until the consumer ships.
- Daemon-restart recovery. The tracker is in-memory only; a crash between the projection decision and the signal dispatch leaves orphan subprocesses outliving the preempt window. Recovery via a pidfile or `/proc` scan is a separate followup.

Until the consumer is wired, an over-budget subprocess still runs to natural completion under the existing post-completion accounting; the daemon then rejects further dispatch via `BudgetExhausted`. The hard-guarantee framing applies only after every item in *Not yet wired* lands.

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
