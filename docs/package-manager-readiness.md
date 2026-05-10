# Package-Manager Readiness

This document defines the readiness contract for public package-manager distribution. It does not create package manifests, tag releases, sign artifacts, upload files, or publish packages.

Run the read-only report from the repository root:

```bash
node agent-os/scripts/package-manager-readiness.mjs --json
```

Validate the report contract:

```bash
node agent-os/scripts/validate-package-manager-readiness.mjs
```

The report uses schema `covenant.package-manager-readiness.v1`.

## Readiness Boundary

`ready_for_manifest_review` means the repository has structured local evidence for package-manager requirements. It does not mean public package distribution is ready.

`ready_for_public_packages` must remain false until package manifests, checksums, install/uninstall CI, signing verification, artifact hosting, and publication approval are recorded.

## Required Decisions

| Requirement | Required before public packages | Current state |
|---|---|---|
| Homebrew manifest | Formula source URL, checksum, install/service behavior, uninstall behavior, and upgrade test evidence. | Planned. |
| Nix manifest | Flake or derivation, hashes, supported platforms, binary paths, and service module behavior. | Planned. |
| Linux packages | Debian and RPM metadata, file ownership, service integration, uninstall behavior, and upgrade behavior. | Planned. |
| Artifact source | Approved release artifact hosting, release manifest digest binding, and package checksum evidence. | Human-owned blocker. |
| Signing verification | Project signing key custody, signature publication, revocation policy, and package install verification. | Human-owned blocker. |
| Install and uninstall CI | Package install, uninstall, upgrade, and rollback checks across supported package formats. | Planned. |
| Publication approval | Registry accounts, publication destinations, announcement language, and rollback announcement policy. | Human-owned blocker. |

## Acceptance Criteria

Public package-manager readiness requires evidence that:

- every supported package channel has a manifest checked into the repository;
- each manifest binds to an immutable release artifact and checksum;
- install, uninstall, upgrade, and rollback behavior are validated in CI;
- package install paths verify release signatures once signing keys are approved;
- publication destinations and rollback policy are approved by a human operator;
- distribution readiness links the accepted package-manager evidence.

Autonomous agents may prepare manifests, validators, CI jobs, and candidate package metadata. Humans retain authority over artifact hosting, release signing key custody, package registry ownership, publication timing, and public release announcements.
