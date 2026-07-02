# Packaging covguard

Three install paths, all pointing at the same signed release built by
`.github/workflows/release-guard.yml` on a `covguard-v*` tag.

## 1. curl | sh

`install.sh` resolves the latest `covguard-*` release asset for the caller's
platform, verifies its checksum, and drops `covguard` on the PATH. Host it at
`opencovenant.org/guard/install.sh`:

```
curl -fsSL https://opencovenant.org/guard/install.sh | sh
```

Shipped targets: `darwin-arm64`, `linux-x86_64`. Other platforms build from
source.

## 2. Homebrew

`covenant-guard.rb` goes in a tap repo (`open-covenant/homebrew-tap`) as
`Formula/covenant-guard.rb`:

```
brew install open-covenant/tap/covenant-guard
```

## 3. cargo

```
cargo install --path agent-os/crates/covenant-guard
# or, once published:  cargo install covenant-guard
```

## Cutting a release

1. Bump `version` in `agent-os/crates/covenant-guard/Cargo.toml` and this
   formula.
2. Tag: `git tag covguard-v0.1.0 && git push origin covguard-v0.1.0`.
3. `release-guard.yml` builds `darwin-arm64` + `linux-x86_64`, cosign-signs each
   tarball and `checksums.txt`, and publishes the release.
4. Read the two `*.tar.gz.sha256` values from the release and paste them into
   the formula's `REPLACE_WITH_*` slots; push the tap.

`install.sh` needs no manual step — it reads the checksums from the release.
