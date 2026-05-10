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

`clean_existing_install` means the prefix is ready for an operator-reviewed source reinstall. It is not automatic upgrade safety. The preflight is read-only and deliberately reports `ready_for_automatic_rollback: false` until the installer records restorable backups, verifies restored binary digests against the previous manifest, and emits rollback audit evidence.

## Validation

Validate the dry-run contract:

```bash
node agent-os/scripts/validate-source-installer.mjs
```

Validate the upgrade preflight contract:

```bash
node agent-os/scripts/validate-source-install-upgrade-plan.mjs
```

The scripts-only gate runs the same check:

```bash
bash agent-os/scripts/validate.sh --scripts
```
