# 0003: Runtime Sandbox Boundary

## Status

Accepted for implementation planning.

## Context

Covenant currently runs agents as local subprocesses with timeout enforcement. That is useful for trusted local automation, but it is not a sandbox-grade boundary for untrusted agents. A production agent-native operating layer needs a stronger isolation backend before it can safely claim to execute arbitrary third-party agents.

The first sandbox backend must be practical for local development, inspectable, and compatible with the existing manifest/runtime model. It must also avoid overclaiming on macOS, where the current subprocess mode should remain explicitly trusted-local.

Primary references:

- gVisor `runsc` OCI runtime and security model: https://gvisor.dev/docs/user_guide/quick_start/oci/ and https://gvisor.dev/docs/architecture_guide/intro/
- bubblewrap project: https://github.com/containers/bubblewrap
- Firecracker microVMs and jailer: https://firecracker-microvm.github.io/, https://github.com/firecracker-microvm/firecracker/blob/main/docs/getting-started.md, and https://github.com/firecracker-microvm/firecracker/blob/main/docs/jailer.md

## Decision

Use **gVisor `runsc` as the first production sandbox backend on Linux**.

Keep the existing subprocess runner as `trusted-local` mode on macOS and on Linux systems without a configured sandbox backend. Do not describe subprocess mode as sandbox-grade isolation.

Treat bubblewrap as a possible future lightweight confinement profile, not the first production security boundary. Treat Firecracker as a future high-isolation backend for longer-running or remote worker pools, not the first local backend.

## Comparison

| Backend | Strengths | Weaknesses | Decision |
| --- | --- | --- | --- |
| gVisor `runsc` | OCI runtime integration, syscall interception boundary, compatible with container images, practical local Linux target. | Linux-only, syscall compatibility gaps, requires container bundle/runtime plumbing. | First production Linux backend. |
| bubblewrap | Small, daemonless, fast, useful for filesystem and namespace confinement. | Shares host kernel directly; depends on Linux namespace configuration; weaker story for hostile code. | Future lightweight profile only. |
| Firecracker | Strong VM boundary, KVM isolation, production jailer model. | Requires KVM, kernel/rootfs lifecycle, networking setup, and heavier image management. | Future high-isolation backend. |

## Initial Architecture

Add a runtime sandbox abstraction with explicit modes:

- `trusted-local`: existing subprocess execution, timeout enforced, no sandbox claim.
- `linux-gvisor`: Linux-only OCI bundle execution through `runsc`.
- `disabled`: explicit operator refusal for agents requiring sandbox-grade isolation when no backend is available.

Agent manifests should be able to declare a sandbox requirement. The daemon must reject an agent that requires sandbox-grade isolation if the host cannot satisfy it.

The runtime contract should preserve the existing stdin/stdout JSON-line protocol. The sandbox backend should only change process isolation and mounted filesystem/network policy, not agent protocol semantics.

## Required Implementation Steps

1. Add manifest fields for sandbox requirement and network/filesystem policy.
2. Add a runtime `SandboxBackend` boundary with `TrustedLocalRunner` as the existing implementation.
3. Add a `GvisorRunner` that prepares an OCI bundle with a minimal rootfs policy and invokes `runsc`.
4. Add daemon config for allowed backends and default mode.
5. Add validation that rejects sandbox-required agents when only trusted-local execution exists.
6. Add live Linux-only tests gated on `runsc` availability.
7. Update public docs so macOS remains trusted-local until a real macOS sandbox backend exists.

## Security Requirements

- No host home directory mount by default.
- No ambient network by default; network policy must be manifest-visible.
- No secret environment inheritance except explicitly allowed variables.
- Runtime logs must not include local absolute home paths or secret values.
- A sandbox-required manifest must fail closed if the configured backend is unavailable.
- Public docs must not claim sandbox-grade isolation until `linux-gvisor` exists and has live coverage.

## Consequences

- The first production sandbox path is Linux-only.
- macOS remains usable for trusted local development but is explicitly not the security boundary for untrusted agents.
- Firecracker remains available as a later backend without forcing VM image management into the first implementation.
- The next engineering slice should implement the manifest/runtime interface before wiring `runsc`.
