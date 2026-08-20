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
- The public live-test inventory includes an ignored Linux gVisor dispatch test gated on `runsc` and an explicit rootfs.
- A repeatable Linux runner guide documents the host, `runsc`, rootfs, and CI adoption requirements for that live path.
- Sandbox stderr redacts configured host-local paths before surfacing failure text.
- The trusted-local subprocess runner starts agents from a cleared environment and passes only `PATH`, `HOME`, and Covenant's own `COVENANT_*` configuration instead of inheriting the daemon's full environment. Operator secrets exported into the daemon's environment are not handed to agents; the capability-gated, audited broker (`GetSecret`) is the sanctioned channel for an agent that needs one.

Not implemented:

- `outbound-https-only`, `full`, `ephemeral`, and `host` sandbox policies are not enforced by the initial runner; they fail closed instead.
- macOS execution is trusted-local only.
- Default CI does not run live sandbox validation on every PR — only sandbox-runtime path PRs trigger the dedicated `.github/workflows/gvisor-live.yml` workflow, which provisions `runsc` and the rootfs, and that workflow is not yet a required check.

## Trust Boundary

The trusted-local runner is useful for first-party automation and local development. It is not a security boundary against hostile agent code.

Trusted-local protects:

- Covenant-mediated state mutations through daemon-side capability checks.
- Runtime budget enforcement via a projection tick that hard-preempts an agent on budget exhaustion by default, and optionally on linear projected overshoot, with a wall-clock kill at `cpu_ms_per_task` as the final backstop.
- Agent protocol attribution because the daemon chooses which manifest produced a result.
- Ambient operator secrets in the daemon's environment: the runner clears the child environment and passes only `PATH`, `HOME`, and `COVENANT_*` configuration, so an exported `ANTHROPIC_API_KEY` or `HERMES_API_KEY` is withheld and the broker (`GetSecret`) is the sanctioned channel for an agent that needs one. This is least-privilege, not a sandbox.

Trusted-local does not protect:

- Host filesystem reads available to the operator user.
- Same-user environment introspection: the runner withholds the daemon's environment from agents, but a hostile agent running as the operator's user can still read another process's environment through `/proc`. The scrub removes the ambient hand-off, not same-user introspection.
- Network access beyond whatever the host OS allows.
- Memory, CPU, syscall, or device abuse within the runtime budget window.
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

## Budget-Driven Preempt

The runtime hard-preempts a subprocess whose owning agent has exhausted its remaining credit budget, killing the in-flight process *before* it completes naturally. This is a stronger guarantee than the wall-clock kill on `cpu_ms_per_task` elapsed, which still fires as the final backstop. Operators can additionally opt into preempting a not-yet-exhausted agent whose linear debit-rate extrapolation projects an overshoot before completion (see the projection policy below). The end-to-end path is covered by the opt-in `live_runtime_preempt` test (`agent-os/crates/covenantd/tests/live_runtime_preempt.rs`), which spawns a fixture binary against a real daemon, pins `budget=1` and `cpu_ms_per_task=10000`, dispatches a single intent, and polls the audit feed for the `BudgetPreempted` row before the binary's wall-clock budget would have fired.

Shipped end-to-end:

- `covenant-runtime::SubprocessRunner` and `GvisorRunner` spawn the agent (or the runsc host process, for gVisor) in a new POSIX process group via `process_group(0)` on Unix so a subsequent `kill(-pid, SIG)` targets every grandchild the agent forked. The configuration is a no-op on non-Unix; the platform gap remains.
- `covenant-runtime::SubprocessTracker` and `TrackedSubprocess` are the primitives the daemon uses to look up a running subprocess's pid by intent id. The daemon constructs the tracker once at startup via `SubprocessTracker::with_persistence($COVENANT_HOME/runtime/subprocess-tracker.jsonl)` and passes it into both runners; on every successful spawn the runner registers an entry keyed by `intent.id` and the entry is unregistered when the dispatch returns (success, error, or timeout-driven kill). Each register/unregister mirrors the live set to the JSONL snapshot (tempfile + atomic rename, best-effort: a persist failure is logged and never blocks the spawn), so a restart can re-adopt still-running subprocesses — see the restart-recovery item below.
- `covenant-runtime::preempt_subprocess_pg(pid, grace)` is the pure async dispatcher that sends `SIGTERM` to a process group, polls every 50ms up to `grace` for the group to drain via `kill(-pid, 0)==ESRCH`, then sends `SIGKILL` if any member is still alive. It returns a `PreemptOutcome` the daemon-side audit emitter maps to `BudgetPreempted` or `BudgetPreemptFailed` rows.
- `covenant-budget::project_overshoot(...)` is the pure projection function the daemon-side tick calls to decide whether an in-flight subprocess is on track to exceed its remaining budget. `BudgetProjectionPolicy::NoExtrapolation` preserves the post-completion-only behavior; `LinearExtrapolation` short-circuits below either an observation-window or sample-count threshold to avoid spurious early triggers.
- `Server::run_projection_tick_iteration` walks the tracker on each call and, per entry, applies the configured `BudgetProjectionPolicy`: the empty-bucket guarantee (`would_exceed(agent, 1)`) is checked first and is policy-independent, then under `LinearExtrapolation` a not-yet-exhausted agent is projected from its `debit_rate_since` window against `tokens_remaining` via `project_overshoot`. Flagged entries are dispatched to `Server::preempt_intent`. `spawn_projection_tick_driver` wraps this in a `tokio::time::interval` (default period: 250ms, configurable via `COVENANT_BUDGET_PROJECTION_TICK_MS`) and is spawned by the daemon's `main` at startup. The policy is selected at startup via `COVENANT_BUDGET_PROJECTION_POLICY` (`none`/`no_extrapolation`, the default, or `linear`/`linear_extrapolation`); under the linear policy `COVENANT_BUDGET_PROJECTION_MIN_WINDOW_MS` (default `1000`) and `COVENANT_BUDGET_PROJECTION_MIN_SAMPLES` (default `3`) gate the extrapolation so a sub-second initial spike or a single fee-paying call cannot project a runaway. An unrecognized policy or a non-integer threshold fails daemon startup rather than silently downgrading enforcement.
- `Server::preempt_intent` reads the tracker entry for an intent_id, calls `preempt_subprocess_pg` with the grace window, and emits the resulting `BudgetPreempted` or `BudgetPreemptFailed` row. The grace window is read from `COVENANT_BUDGET_PREEMPT_GRACE_MS` (default: `2000` ms). A 0-ms grace immediately escalates to `SIGKILL` and produces audit noise; operators should leave room for `SIGTERM` to land.
- `covenant-audit::AuditKind::BudgetPreempted` and `BudgetPreemptFailed` are wired into `audit_kind_requires_persistence` so the daemon refuses to drop them on audit-write failure.
- `SubprocessTracker::recover()` re-adopts subprocesses persisted by a prior daemon instance so a crash between the projection decision and the signal dispatch no longer orphans them past the preempt window. The daemon's `main` calls it synchronously right after constructing the tracker and **before** `spawn_projection_tick_driver`, so the tick never observes a half-populated tracker. Recovery is signal-safe: it never kills during the scan; it validates each persisted pid's process identity (`pgid` + `/proc` start time, read from `/proc/<pid>/stat`) against the value captured at register time and **re-tracks only an exact match**. A vanished pid (ESRCH) is dropped as benign; a pid whose live identity differs (the pid-reuse case) is refused and never signalled; an unverifiable pid is refused conservatively. Re-tracked survivors are reaped on the next projection tick only if their budget is exhausted, reusing the already-audited preempt path. A missing or corrupt snapshot recovers as empty (one unparseable row cannot strand the rest).

Remaining gap:

- Off-Linux restart recovery. Ownership validation depends on `/proc`, so on macOS/Windows the recovery scan compiles and runs but can prove no pid's identity and therefore re-tracks nothing — an interrupted preempt can still orphan a subprocess there. This mirrors the existing non-Unix preempt platform gap; production daemons run on Linux, where recovery is active.

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
