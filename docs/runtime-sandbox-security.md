# Runtime Sandbox Security Contract

This document defines the security contract for Covenant runtime isolation. It separates the implemented trusted-local runner from the planned Linux gVisor backend so public claims stay aligned with the code.

## Current State

Implemented:

- Agent manifests may declare `[sandbox]`.
- `sandbox.required = true` is rejected unless the manifest names a sandbox-grade backend.
- The current subprocess runner is `trusted-local`; it refuses to run agents that require sandbox-grade isolation.
- Runtime docs and public status identify gVisor execution as planned, not implemented.

Not implemented:

- No gVisor runner exists yet.
- No OCI bundle builder exists yet.
- No sandbox filesystem, network, environment, or cgroup policy is enforced yet.
- macOS execution is trusted-local only.

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

This invariant is already enforced by `covenant-manifest` validation and `covenant-runtime` subprocess dispatch.

## First Production Backend

The accepted first production sandbox backend is Linux gVisor through `runsc`. The backend must meet this minimum contract before Covenant can claim sandbox-grade local execution:

- prepare an OCI bundle from the agent package without mounting the host home directory;
- mount the agent package read-only unless the manifest explicitly requests an ephemeral writable layer;
- start with no ambient host secrets or arbitrary inherited environment variables;
- enforce manifest-visible network policy;
- enforce wall-clock timeout and kill the sandbox on timeout;
- surface sandbox setup failure as dispatch failure, not fallback execution;
- redact host-local paths and secret-looking values from operator-facing logs;
- provide live Linux-only validation gated on `runsc` availability.

## Filesystem Policy

Manifest values are parsed now and enforced by future sandboxed backends:

| Policy | Meaning |
| --- | --- |
| `read-only-package` | Agent package is visible read-only. This is the default. |
| `ephemeral` | Agent receives a writable scratch layer that is discarded after dispatch. |
| `host` | Host filesystem access. This must be treated as privileged and should require explicit operator approval before use by untrusted agents. |

Until a sandbox backend exists, these values are declarative only. They must not be described as enforced outside sandboxed runtime paths.

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
