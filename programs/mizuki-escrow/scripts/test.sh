#!/usr/bin/env bash
set -euo pipefail

program_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
build_sbf="${CARGO_BUILD_SBF_BIN:-cargo-build-sbf}"
artifact="$program_root/target/deploy/mizuki_escrow_program.so"
build_log="$(mktemp -t mizuki-escrow-build.XXXXXX)"
trap 'rm -f "$build_log"' EXIT

if [[ "$build_sbf" == */* ]]; then
  test -x "$build_sbf"
else
  command -v "$build_sbf" >/dev/null
fi

version_output="$("$build_sbf" --version)"
printf '%s\n' "$version_output"
build_version="$(printf '%s\n' "$version_output" | awk '$1 == "cargo-build-sbf" || $1 == "solana-cargo-build-sbf" { print $2; exit }')"
platform_version="$(printf '%s\n' "$version_output" | awk '$1 == "platform-tools" { sub(/^v/, "", $2); print $2; exit }')"

if [[ ! "$build_version" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || (( ${build_version%%.*} < 4 )); then
  echo "cargo-build-sbf 4.0.0 or newer is required for SBPFv3" >&2
  exit 1
fi

platform_major="${platform_version%%.*}"
platform_rest="${platform_version#*.}"
platform_minor="${platform_rest%%.*}"
if [[ ! "$platform_version" =~ ^[0-9]+\.[0-9]+([.][0-9]+)?$ ]] ||
  (( platform_major < 1 || (platform_major == 1 && platform_minor < 53) )); then
  echo "platform-tools 1.53 or newer is required for SBPFv3" >&2
  exit 1
fi

cargo fmt --manifest-path "$program_root/Cargo.toml" -- --check
cargo test --locked --manifest-path "$program_root/Cargo.toml" --lib
"$build_sbf" --arch v3 --manifest-path "$program_root/Cargo.toml" -- --locked 2>&1 | tee "$build_log"

if rg -i 'undefined symbols?|not known.*run-time error' "$build_log"; then
  echo "unresolved SBPF symbol detected" >&2
  exit 1
fi

test -f "$artifact"
elf_flags="$(od -An -tx1 -j 48 -N 4 "$artifact" | tr -d '[:space:]')"
if [[ "$elf_flags" != "03000000" ]]; then
  echo "expected SBPFv3 ELF flags, found 0x$elf_flags" >&2
  exit 1
fi

cargo test --locked --manifest-path "$program_root/Cargo.toml" --lib --features sbf-test
cargo clippy --locked --manifest-path "$program_root/Cargo.toml" --all-targets --all-features -- -D warnings
