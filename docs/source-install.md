# Source Install

The source installer builds the daemon and CLI from the local checkout and copies them into an operator-selected prefix. It is the recommended path when you want to build from a specific commit, when you are developing against the repo, or when no signed release exists yet for your platform.

For tagged releases with signed prebuilt binaries, see [RELEASES.md](../RELEASES.md) and the GitHub Releases page. Source install is not a package-manager release, SDK stability commitment, or automatic upgrade path.

## Dry Run

Preview the write plan without building or copying files:

```bash
node agent-os/scripts/install-source.mjs --prefix /tmp/covenant --dry-run --json
```

The dry-run output uses schema `covenant.source-install.v1` and lists only three writes under the prefix:

- `bin/covenantd`
- `bin/covenant`
- `share/covenant/install-manifest.json`

## Install

From the repository root:

```bash
node agent-os/scripts/install-source.mjs --prefix /tmp/covenant --profile release
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
node agent-os/scripts/source-install-upgrade-plan.mjs --prefix /tmp/covenant --json
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
node agent-os/scripts/source-install-rollback.mjs --prefix /tmp/covenant --json
```

Apply the rollback only after the dry run reports `ready: true`:

```bash
node agent-os/scripts/source-install-rollback.mjs --prefix /tmp/covenant --apply --json
```

Rollback verifies every backup digest before copying files back into place. Applied rollback writes local evidence under `share/covenant/rollback-reports/`. This is local source-install rollback, not package-manager rollback, signed release rollback, or public upgrade policy.

## Homebrew (Head Formula)

A HEAD-only Homebrew formula lives at `Formula/covenant.rb`. It builds the daemon and CLI from the `main` branch of this repository, so it is a Homebrew-wrapped source build, not a tagged release. There is no bottle, no checksum, and no signature: Homebrew clones the repo at HEAD and compiles locally.

Install from a clone of this repository:

```bash
brew install --HEAD --formula Formula/covenant.rb
```

The formula builds `covenantd` and `covenant` with the same locked release profile as the source installer (`cargo build -p covenantd -p covenant --locked --release` from `agent-os/`) and links both binaries into the Homebrew prefix. It declares a build dependency on Homebrew's `rust`.

Run the formula test:

```bash
brew test covenant
```

Run the daemon as a launchd service:

```bash
brew services start covenant
```

Under the service, the daemon reads its data directory from `COVENANT_HOME`, set to the Homebrew `var/covenant` directory. For manual runs, `COVENANT_HOME` defaults to `~/.covenant`.

This is a convenience channel for tracking `main`. It is not a registered tap, ships no bottles, verifies no signatures, and is not bound to a release artifact. For a build pinned to a specific commit or a signed release, use the source install above or see [RELEASES.md](../RELEASES.md).

## Nix (Head Flake)

A HEAD-only Nix flake lives at `flake.nix`. It builds the daemon and CLI from the working tree with `buildRustPackage` (dependencies vendored from `agent-os/Cargo.lock`), so it is a Nix-wrapped source build, not a tagged release. There is no pinned derivation hash, no binary cache, and no signature.

Build from a clone of this repository:

```bash
nix build .#covenant
```

Or install into your profile:

```bash
nix profile install .#covenant
```

The flake targets `x86_64-linux`, `aarch64-linux`, `x86_64-darwin`, and `aarch64-darwin`, builds the same `covenantd` and `covenant` binaries as the source installer, and carries `pkg-config`/`openssl` build inputs for the CLI's openssl-sys dependency chain, so the build succeeds in the sealed Nix sandbox instead of relying on a system OpenSSL.

Profile upgrades and rollbacks are real package operations on this channel:

```bash
nix profile upgrade covenant
nix profile rollback
```

On NixOS, `nixosModules.covenant` runs `covenantd` as a systemd service with `COVENANT_HOME` pinned to the managed state directory (`/var/lib/covenant`):

```nix
{
  imports = [ covenant.nixosModules.covenant ];
  services.covenant.enable = true;
}
```

For manual runs, `COVENANT_HOME` defaults to `~/.covenant`.

This is a convenience channel for tracking the checkout. Publication of the flake, nixpkgs submission, derivation hash pinning, and cache signing remain operator-owned; for a signed release, see [RELEASES.md](../RELEASES.md).

## Debian and RPM (Head Packaging Templates)

HEAD-only packaging templates live at `debian/` and `covenant.spec`. Both build the same `covenantd` and `covenant` binaries as the source installer (`cargo build -p covenantd -p covenant --locked --release` from `agent-os/`) and install a `covenantd` systemd unit with the same contract as the Nix module: `COVENANT_HOME` pinned to `/var/lib/covenant`, `StateDirectory=covenant`, `Restart=always`.

Build a native Debian package from a clone on `amd64` or `arm64`:

```bash
dpkg-buildpackage -us -uc -b
```

Build an RPM from the working tree on `x86_64` or `aarch64`:

```bash
rpmbuild --build-in-place -bb covenant.spec
```

These are templates, not published packages: no archive URLs, no checksums, no signatures, no repositories. Both builds fetch crates from the network (the tree vendors nothing), so they run on a networked machine and are not sbuild/pbuilder/mock-compatible without crate vendoring. CI builds, installs, and smoke-checks both packages on packaging changes (`.github/workflows/packaging.yml`). The Debian scriptlets defer systemd lifecycle handling to debhelper, and the spec uses the standard `%systemd_post`/`%systemd_preun`/`%systemd_postun_with_restart` macros. Repository hosting, signing keys, and uploads remain operator-owned; for a signed release, see [RELEASES.md](../RELEASES.md).

## Validation

Run the public guard:

```bash
bash agent-os/scripts/validate.sh --scripts
```

Detailed installer, upgrade-preflight, and rollback contract validators are maintained internally.
