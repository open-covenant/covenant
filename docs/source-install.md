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

## Validation

Validate the dry-run contract:

```bash
node agent-os/scripts/validate-source-installer.mjs
```

The scripts-only gate runs the same check:

```bash
bash agent-os/scripts/validate.sh --scripts
```
