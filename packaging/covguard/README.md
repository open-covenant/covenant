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

Shipped targets: `darwin-arm64` and `linux-x86_64`. Other platforms build from
source. The Linux sandbox needs bubblewrap installed at runtime.

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
3. `release-guard.yml` builds `darwin-arm64` and `linux-x86_64`, cosign-signs
   the tarballs and `checksums.txt`, and publishes the release.
4. Copy the two sha256 values from the release's `checksums.txt` into the
   formula's `REPLACE_WITH_DARWIN_ARM64_SHA256` and `REPLACE_WITH_LINUX_X86_64_SHA256`
   slots; push the tap.

`install.sh` needs no manual step: it reads and verifies against
`checksums.txt` from the release.
