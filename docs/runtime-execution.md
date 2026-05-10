# Runtime Execution

Covenant runs agents through a runtime backend selected at daemon startup. The trusted-local runner is the default for first-party automation; the Linux gVisor runner is opt-in until host provisioning, runner metadata, and CI failure policy are accepted.

This page indexes the runtime execution contract docs and surfaces the per-precondition CI promotion signal.

## Contract Documents

- [Runtime sandbox security contract](./runtime-sandbox-security.md): trust boundary, supported sandbox policies, and the trusted-local versus Linux gVisor split.
- [Linux gVisor live runner setup](./gvisor-live-runner.md): host provisioning, `runsc` configuration, rootfs requirements, and the opt-in live test.
- [gVisor host readiness](./gvisor-host-readiness.md): the `covenant.gvisor-host-readiness.v1` report and the pinned `covenant.gvisor-runner-metadata.v1` runner contract.

## Promotion Readiness Report

The promotion readiness report aggregates the host readiness gates and runner metadata fields into a per-precondition CI promotion signal. It does not install `runsc`, build a rootfs, run live tests, change CI configuration, or accept a runner metadata record.

Run the read-only report from the repository root:

```bash
node agent-os/scripts/gvisor-runner-promotion-readiness.mjs --json
```

Validate the report contract:

```bash
node agent-os/scripts/validate-gvisor-runner-promotion-readiness.mjs
```

The report uses schema `covenant.gvisor-runner-promotion-readiness.v1`. It binds the upstream `covenant.gvisor-host-readiness.v1` report and the `covenant.gvisor-runner-metadata.v1` runner contract.

Preconditions reported per gate:

- `host-provisioning`: Linux host, `runsc` runtime, and rootfs shell evidence pass on the executing host.
- `runsc-image-digest`: a pinned `runner_metadata.runsc.source` and `runner_metadata.runsc.digest_sha256` are recorded.
- `rootfs-digest`: a pinned `runner_metadata.rootfs.source` and `runner_metadata.rootfs.digest_sha256` are recorded.
- `host-architecture-pinned`: `runner_metadata.host.kernel` and `runner_metadata.rootfs.architecture` are recorded.
- `failure-policy`: `runner_metadata.policy.failure_mode` and `runner_metadata.policy.unsupported_host_policy` are approved.
- `runner-record-accepted`: `runner_metadata.status` is `accepted` (currently `unpinned`).

Promotion remains blocked while any precondition is blocked. Strict mode (`--strict`) exits non-zero when promotion is blocked, so a future CI step can fail loudly when a runner metadata change accidentally regresses readiness.

## Human Authority

CI host provisioning, runner image selection, rootfs artifact provenance, kernel/architecture baseline, sandbox failure policy, and required-job scope all remain human-owned. Automation may surface the readiness state and prepare candidate metadata records. It must not pin runner image digests, modify CI configuration, or accept the runner metadata record without an approved human decision.
