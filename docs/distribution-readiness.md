# Distribution Readiness

Covenant currently has a source-built alpha install path. Public package distribution, signed release artifacts, SDK stability commitments, and automatic upgrade safety are separate graduation gates.

Run the read-only readiness report from the repository root:

```bash
node agent-os/scripts/distribution-readiness.mjs --json
```

Validate the report contract:

```bash
node agent-os/scripts/validate-distribution-readiness.mjs
```

The report uses schema `covenant.distribution-readiness.v1`. It does not tag, sign, publish, upload artifacts, create package-manager manifests, or change SDK package state.

## Gates

| Gate | Current state | Evidence | Human boundary |
|---|---|---|---|
| `source-alpha-install` | Implemented | `agent-os/scripts/install-source.mjs`, `agent-os/scripts/validate-source-installer.mjs`, `docs/source-install.md` | No publication decision. |
| `source-upgrade-preflight` | Implemented | `agent-os/scripts/source-install-upgrade-plan.mjs`, `agent-os/scripts/validate-source-install-upgrade-plan.mjs`, `docs/source-install.md` | Operator review before reinstalling over an existing prefix. |
| `package-manager-distribution` | Planned | None yet | Artifact destinations and publication approval. |
| `signed-release-artifacts` | Planned | `docs/provenance/release-subjects.md` | Project signing key custody, signature publication, and revocation policy. |
| `sdk-stability` | Planned | `packages/sdk/README.md`, `packages/sdk-ui/README.md` | SDK stability commitment and publication approval. |
| `upgrade-policy` | Experimental | Source upgrade preflight only | Public upgrade, automatic rollback, and rollback audit language. |

`ready_for_source_alpha` can be true while `ready_for_public_distribution` is false. That is the expected alpha state until package-manager installation, release signing, SDK compatibility, and upgrade rollback evidence exist.

## Graduation Work

Public distribution can move forward only after:

- package-manager manifests and install/uninstall CI coverage exist;
- release artifact subjects bind file digests, validation evidence, and the release id;
- project signing key custody and rotation policy are approved;
- SDK semantic versioning and compatibility fixtures are defined;
- upgrade, rollback, and installer migration checks are documented and tested beyond the current read-only source install preflight.

Automation may prepare these checks and evidence. Humans still own public publication, signing, SDK stability claims, and release announcements.
