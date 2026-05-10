# Source Install

Covenant alpha releases are source-built local infrastructure releases. The source installer builds the daemon and CLI from the local checkout and copies them into an operator-selected prefix.

It is not a package-manager release, signed artifact, SDK stability commitment, or automatic upgrade path.

## Dry Run

Preview the write plan without building or copying files:

```bash
node agent-os/scripts/install-source.mjs --prefix /tmp/covenant-alpha --dry-run --json
```

The dry-run output uses schema `covenant.source-install.v1` and lists only three writes under the prefix:

- `bin/covenantd`
- `bin/covenant`
- `share/covenant/install-manifest.json`

## Install

From the repository root:

```bash
node agent-os/scripts/install-source.mjs --prefix /tmp/covenant-alpha --profile release
```

The installer runs:

```bash
cargo build -p covenantd -p covenant --locked --release
```

Then it copies the built binaries into the prefix and writes `share/covenant/install-manifest.json`.

The manifest records schema, source commit, build profile, binary names, crate names, repository-relative target paths, prefix-relative installed paths, byte counts, and SHA-256 digests. It deliberately stores relative installed paths rather than machine-local home directories.

## Upgrade Preflight

Inspect a prefix before reinstalling over an existing source install:

```bash
node agent-os/scripts/source-install-upgrade-plan.mjs --prefix /tmp/covenant-alpha --json
```

The report uses schema `covenant.source-install-upgrade-plan.v1`. It reuses the installer dry-run plan, reads the existing install manifest when present, checks whether planned binary replacements match the recorded manifest digests, and classifies the prefix as:

- `fresh_install`
- `clean_existing_install`
- `partial_existing_install`
- `drifted_existing_install`

`clean_existing_install` means the prefix is ready for an operator-reviewed source reinstall. It is not package-manager upgrade safety. The preflight reports whether the current source install already has a restorable rollback checkpoint, but it does not mutate the prefix.

## Rollback

When `install-source.mjs` replaces an existing source install, it preserves the previous binaries and manifest under:

```text
share/covenant/backups/<checkpoint-id>/
```

The new install manifest records a `rollback_checkpoint` with prefix-relative backup paths, byte counts, SHA-256 digests, and file modes. Restore the checkpoint with a dry run first:

```bash
node agent-os/scripts/source-install-rollback.mjs --prefix /tmp/covenant-alpha --json
```

Apply the rollback only after the dry run reports `ready: true`:

```bash
node agent-os/scripts/source-install-rollback.mjs --prefix /tmp/covenant-alpha --apply --json
```

Rollback verifies every backup digest before copying files back into place. Applied rollback writes local evidence under `share/covenant/rollback-reports/`. This is local source-install rollback, not package-manager rollback, signed release rollback, or public upgrade policy.

## Validation

Validate the dry-run contract:

```bash
node agent-os/scripts/validate-source-installer.mjs
```

Validate the upgrade preflight contract:

```bash
node agent-os/scripts/validate-source-install-upgrade-plan.mjs
```

Validate rollback checkpoints:

```bash
node agent-os/scripts/validate-source-install-rollback.mjs
```

The scripts-only gate runs the same check:

```bash
bash agent-os/scripts/validate.sh --scripts
```
