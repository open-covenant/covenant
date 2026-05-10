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

The report also emits `package_manager_manifest` with schema `covenant.package-manager-manifest.v1`. This is a draft contract, not a publishable manifest. It names the required fields for Homebrew, Nix, Debian, and RPM channels while leaving every channel placeholder empty until real package manifests, immutable artifact URLs, checksums, signature verification, and install/upgrade/rollback checks exist.

Required manifest fields:

- `channel`
- `package_name`
- `manifest_path`
- `artifact_url`
- `artifact_sha256`
- `signature_verification`
- `install_check`
- `uninstall_check`
- `upgrade_check`
- `rollback_check`

The validator keeps `ready_for_manifest_review` false while placeholders are empty and rejects machine-local paths in the manifest contract.

## Homebrew Section

`package_manager_manifest.homebrew` extends the generic channel placeholder with formula-specific fields. It uses schema `covenant.package-manager-manifest-homebrew.v1` and stays in `draft_empty_placeholders` with `ready_for_homebrew_review` false until a real formula, immutable artifact URL, checksum, signature verification, and install/upgrade/rollback evidence exist. The validator rejects machine-local paths and any non-null placeholder.

Required homebrew fields:

- `tap`
- `formula_path`
- `formula_class_name`
- `formula_version`
- `artifact_url`
- `artifact_sha256`
- `bottle`
- `caveats`
- `service_definition`
- `test_block`
- `livecheck`
- `install_check`
- `uninstall_check`
- `upgrade_check`
- `rollback_check`
- `signature_verification`

`bottle` carries `enabled`, `cellar`, and an empty `bottles` array until prebuilt-bottle decisions are accepted. Homebrew tap publication, formula custody, and signing keys remain human-owned.

## Nix Section

`package_manager_manifest.nix` extends the generic channel placeholder with flake/derivation-specific fields. It uses schema `covenant.package-manager-manifest-nix.v1` and stays in `draft_empty_placeholders` with `ready_for_nix_review` false until a real flake or derivation, pinned source, derivation hash, supported platforms, and install/upgrade/rollback evidence exist. The validator rejects machine-local paths and any non-null placeholder.

Required nix fields:

- `flake_source`
- `flake_ref`
- `derivation_path`
- `derivation_hash`
- `package_name`
- `package_version`
- `platforms`
- `binary_path`
- `service_module`
- `install_check`
- `uninstall_check`
- `upgrade_check`
- `rollback_check`
- `signature_verification`

`platforms` is an empty array until supported targets (such as `x86_64-linux` or `aarch64-darwin`) are accepted. Nix flake or channel publication, derivation custody, and signing keys remain human-owned.

## Debian Section

`package_manager_manifest.debian` extends the generic channel placeholder with control-file and scriptlet fields. It uses schema `covenant.package-manager-manifest-debian.v1` and stays in `draft_empty_placeholders` with `ready_for_debian_review` false until real control metadata, dependencies, scriptlets, file ownership, service unit, and install/upgrade/rollback evidence exist. The validator rejects machine-local paths and any non-null placeholder.

Required debian fields:

- `package_name`
- `package_version`
- `control_metadata`
- `depends`
- `recommends`
- `suggests`
- `architectures`
- `file_ownership`
- `service_unit`
- `postinst`
- `prerm`
- `postrm`
- `install_check`
- `uninstall_check`
- `upgrade_check`
- `rollback_check`
- `signature_verification`

`control_metadata` carries `section`, `priority`, `maintainer`, `homepage`, and `description`, each null until decided. `depends`, `recommends`, `suggests`, and `architectures` are empty arrays until concrete decisions land. Debian repository hosting, signing keys, and uploads remain human-owned.

## RPM Section

`package_manager_manifest.rpm` extends the generic channel placeholder with spec-file and scriptlet fields. It uses schema `covenant.package-manager-manifest-rpm.v1` and stays in `draft_empty_placeholders` with `ready_for_rpm_review` false until real spec metadata, requires, scriptlets, file ownership, service unit, and install/upgrade/rollback evidence exist. The validator rejects machine-local paths and any non-null placeholder.

Required rpm fields:

- `package_name`
- `package_version`
- `spec_metadata`
- `requires`
- `recommends`
- `suggests`
- `architectures`
- `file_ownership`
- `service_unit`
- `pre_scriptlet`
- `post_scriptlet`
- `preun_scriptlet`
- `postun_scriptlet`
- `install_check`
- `uninstall_check`
- `upgrade_check`
- `rollback_check`
- `signature_verification`

`spec_metadata` carries `summary`, `license`, `url`, `group`, and `vendor`, each null until decided. `requires`, `recommends`, `suggests`, and `architectures` are empty arrays until concrete decisions land. RPM repository hosting, signing keys, and uploads remain human-owned.

## Gates

| Gate | Current state | Evidence | Human boundary |
|---|---|---|---|
| `source-alpha-install` | Implemented | `agent-os/scripts/install-source.mjs`, `agent-os/scripts/validate-source-installer.mjs`, `docs/source-install.md` | No publication decision. |
| `source-upgrade-preflight` | Implemented | `agent-os/scripts/source-install-upgrade-plan.mjs`, `agent-os/scripts/validate-source-install-upgrade-plan.mjs`, `docs/source-install.md` | Operator review before reinstalling over an existing prefix. |
| `source-rollback-checkpoint` | Implemented | `agent-os/scripts/source-install-rollback.mjs`, `agent-os/scripts/validate-source-install-rollback.mjs`, `docs/source-install.md` | Local source rollback only. |
| `sdk-compatibility-policy` | Implemented | `docs/sdk-compatibility.md`, `agent-os/scripts/sdk-compatibility.mjs`, `agent-os/scripts/validate-sdk-compatibility.mjs` | Public semver and npm publication approval. |
| `package-manager-distribution` | Documented, manifests blocked | `docs/package-manager-readiness.md`, `agent-os/scripts/package-manager-readiness.mjs`, `agent-os/scripts/validate-package-manager-readiness.mjs` | Artifact hosting, signing keys, registry ownership, and publication approval. |
| `signed-release-artifacts` | Planned | `docs/provenance/release-subjects.md` | Project signing key custody, signature publication, and revocation policy. |
| `sdk-stability` | Experimental | Workspace-alpha compatibility policy | SDK stability commitment and publication approval. |
| `upgrade-policy` | Experimental | Source upgrade preflight and local rollback checkpoints | Public package rollback and rollback announcement language. |

`ready_for_source_alpha` can be true while `ready_for_public_distribution` is false. That is the expected alpha state until package-manager installation, release signing, SDK compatibility, and upgrade rollback evidence exist.

## Graduation Work

Public distribution can move forward only after:

- package-manager manifests and install/uninstall/upgrade/rollback CI coverage exist;
- the `covenant.package-manager-manifest.v1` placeholders are replaced by repository manifests bound to immutable artifact URLs and checksums;
- release artifact subjects bind file digests, validation evidence, and the release id;
- project signing key custody and rotation policy are approved;
- SDK semantic versioning and compatibility fixtures are defined;
- package-manager upgrade, package rollback, and installer migration checks are documented and tested beyond local source install rollback.

Automation may prepare these checks and evidence. Humans still own public publication, signing, SDK stability claims, and release announcements.
