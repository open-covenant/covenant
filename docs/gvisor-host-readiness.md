# gVisor Host Readiness

The Linux gVisor runner has an opt-in live test and setup guide. Required CI promotion needs a separate host-readiness contract so unsupported hosts, missing `runsc`, missing rootfs artifacts, and CI policy gaps are visible before sandbox claims change.

Run the read-only readiness report from the repository root:

```bash
node agent-os/scripts/gvisor-host-readiness.mjs --json
```

Validate the report contract:

```bash
node agent-os/scripts/validate-gvisor-host-readiness.mjs
```

The report uses schema `covenant.gvisor-host-readiness.v1`. It does not install `runsc`, build a rootfs, run live tests, change CI, or make gVisor mandatory.

## Gates

| Gate | Current state | Evidence | Human boundary |
|---|---|---|---|
| `linux-host` | Host-dependent | Current platform metadata | No policy decision. |
| `runsc-runtime` | Host-dependent | `runsc --version` when available | No policy decision. |
| `rootfs-shell` | Host-dependent | `COVENANT_LIVE_GVISOR_ROOTFS` with `bin/sh` | No policy decision. |
| `runtime-policy-evidence` | Implemented | `covenant-runtime`, `live_gvisor.rs`, `docs/gvisor-live-runner.md` | No policy decision. |
| `ci-runner-provisioning` | Planned | None yet | Runner image or setup-step approval. |
| `rootfs-provenance` | Planned | None yet | Pinned rootfs artifact approval. |
| `mandatory-ci-policy` | Planned | None yet | Required-job scope and unsupported-host failure policy. |

`ready_for_local_live_gvisor` can be true on a properly provisioned Linux host while `ready_for_required_ci` remains false. That is the expected state until a pinned runner image, rootfs provenance, and CI failure policy are approved.

## CI Promotion Work

Before Linux gVisor dispatch becomes required CI evidence, the project needs:

- a pinned Linux runner image or setup step with `runsc`;
- captured `runsc --version` and kernel/runtime baseline in CI logs;
- a pinned rootfs artifact that includes `/bin/sh` and records architecture compatibility;
- a failure policy that only blocks appropriate sandbox-runtime changes while the host is being stabilized;
- continued fail-closed behavior when `sandbox.required = true` cannot be satisfied.

Automation may prepare checks and reports. Humans still own required-job scope, runner provisioning, rootfs provenance, and public sandbox-readiness claims.
