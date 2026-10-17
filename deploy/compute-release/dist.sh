#!/usr/bin/env bash
# Builds the Covenant Compute command-line tools for one target, packs them
# into dist/<asset>.tar.gz and runs every packed binary once.
#
#   deploy/compute-release/dist.sh <target>
#
# The release workflow runs this for each target; it runs the same way on a
# workstation. Linux builds are static (musl), so one archive runs on any
# distribution.
set -euo pipefail

target="${1:-}"
case "$target" in
  aarch64-apple-darwin) asset=covenant-compute-darwin-arm64 ;;
  x86_64-apple-darwin) asset=covenant-compute-darwin-x86_64 ;;
  x86_64-unknown-linux-musl) asset=covenant-compute-linux-x86_64 ;;
  aarch64-unknown-linux-musl) asset=covenant-compute-linux-arm64 ;;
  *)
    echo "usage: dist.sh <aarch64-apple-darwin | x86_64-apple-darwin | x86_64-unknown-linux-musl | aarch64-unknown-linux-musl>" >&2
    exit 2
    ;;
esac

if [ "$target" = x86_64-apple-darwin ] && [ "$(uname -m)" = arm64 ] && ! arch -x86_64 /usr/bin/true 2>/dev/null; then
  echo "dist.sh: the Intel build is checked under Rosetta; install it with: softwareupdate --install-rosetta" >&2
  exit 1
fi

here="$(cd "$(dirname "$0")" && pwd)"
root="$(cd "$here/../.." && pwd)"
workspace="$root/agent-os"
stake_workspace="$workspace/crates/covenant-compute-lease-signer"
bins=(covenant-compute covenant-compute-mcp covenant-compute-node)

version="$(sed -n 's/^version = "\(.*\)"$/\1/p' "$workspace/crates/covenant-compute-buyer/Cargo.toml")"
[ -n "$version" ] || { echo "dist.sh: no version in covenant-compute-buyer/Cargo.toml" >&2; exit 1; }

export MACOSX_DEPLOYMENT_TARGET="${MACOSX_DEPLOYMENT_TARGET:-11.0}"
export CARGO_PROFILE_RELEASE_STRIP=symbols

cargo build --release --locked --target "$target" --manifest-path "$workspace/Cargo.toml" \
  -p covenant-compute-buyer -p covenant-compute-node \
  --bin covenant-compute --bin covenant-compute-mcp --bin covenant-compute-node
# The stake tool links the Solana SDK, which resolves in its own workspace.
cargo build --release --locked --target "$target" --manifest-path "$stake_workspace/Cargo.toml" \
  --bin covenant-compute-stake

# Ask cargo where each workspace builds, so CARGO_TARGET_DIR and build.target-dir
# are honoured and a stale binary from another location is never packed.
release_dir() {
  local dir
  dir="$(cargo metadata --format-version 1 --no-deps --manifest-path "$1/Cargo.toml" |
    sed -n 's/.*"target_directory":"\([^"]*\)".*/\1/p')"
  if [ -z "$dir" ]; then
    echo "dist.sh: cargo metadata named no target directory for $1" >&2
    exit 1
  fi
  echo "$dir/$target/release"
}
bin_dir="$(release_dir "$workspace")"
stake_bin_dir="$(release_dir "$stake_workspace")"

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

stage="$work/$asset"
mkdir "$stage"
for bin in "${bins[@]}"; do
  install -m 0755 "$bin_dir/$bin" "$stage/"
done
install -m 0755 "$stake_bin_dir/covenant-compute-stake" "$stage/"
install -m 0644 "$root/LICENSE" "$root/NOTICE.md" "$here/README.md" "$stage/"

if tar --version | grep -q 'GNU tar'; then
  tar_flags=(--owner=0 --group=0 --numeric-owner)
else
  tar_flags=(--uid 0 --gid 0 --no-mac-metadata --no-xattrs)
fi
archive="$work/$asset.tar.gz"
COPYFILE_DISABLE=1 tar "${tar_flags[@]}" -C "$work" -czf "$archive" "$asset"

# An Apple silicon machine runs the Intel build under Rosetta.
run_packed() {
  if [ "$target" = x86_64-apple-darwin ] && [ "$(uname -m)" = arm64 ]; then
    arch -x86_64 "$@"
  else
    "$@"
  fi
}

# Check what was packed, not what was built: unpack and run each binary. The
# archive only reaches dist/ once every check has passed.
mkdir "$work/check"
tar -C "$work/check" -xzf "$archive"
for bin in "${bins[@]}" covenant-compute-stake; do
  path="$work/check/$asset/$bin"
  got="$(run_packed "$path" --version)"
  if [ "$got" != "$bin $version" ]; then
    echo "dist.sh: $bin --version printed '$got', expected '$bin $version'" >&2
    exit 1
  fi
  run_packed "$path" --help >/dev/null
  if [[ $target == *-linux-musl ]] && ! file "$path" | grep -q -E 'statically linked|static-pie linked'; then
    echo "dist.sh: $bin is not statically linked: $(file "$path")" >&2
    exit 1
  fi
done

mkdir -p "$root/dist"
mv "$archive" "$root/dist/"
echo "$root/dist/$asset.tar.gz"
