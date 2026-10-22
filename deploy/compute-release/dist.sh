#!/usr/bin/env bash
# Builds the Covenant Compute command-line tools for one target, packs them
# into dist/<asset>.tar.gz and runs every packed binary once.
#
#   deploy/compute-release/dist.sh [--snapshot] <target>
#
# Tags use strict release mode; PRs explicitly select a nonpublishing candidate
# snapshot that may contain different crate versions. Both build and test every
# packed binary. The same modes work on a workstation. Linux builds are static
# (musl), so one archive runs on any distribution.
set -euo pipefail

mode=release
if [ "${1:-}" = --snapshot ]; then
  mode=snapshot
  shift
fi
if [ "$#" -ne 1 ]; then
  echo "usage: dist.sh [--snapshot] <target>" >&2
  exit 2
fi
if [ "$mode" = snapshot ] && { [[ "${GITHUB_REF:-}" == refs/tags/* ]] || [ "${GITHUB_REF_TYPE:-}" = tag ]; }; then
  echo "dist.sh: candidate snapshot mode is forbidden for tag refs" >&2
  exit 2
fi

target="$1"
case "$target" in
  aarch64-apple-darwin) asset=covenant-compute-darwin-arm64 ;;
  x86_64-apple-darwin) asset=covenant-compute-darwin-x86_64 ;;
  x86_64-unknown-linux-musl) asset=covenant-compute-linux-x86_64 ;;
  aarch64-unknown-linux-musl) asset=covenant-compute-linux-arm64 ;;
  *)
    echo "usage: dist.sh [--snapshot] <aarch64-apple-darwin | x86_64-apple-darwin | x86_64-unknown-linux-musl | aarch64-unknown-linux-musl>" >&2
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

manifest_version() {
  local value
  value="$(sed -n 's/^version = "\([^" ]*\)"$/\1/p' "$1")"
  if ! [[ "$value" =~ ^[0-9]+\.[0-9]+\.[0-9]+([-+][0-9A-Za-z.+-]+)?$ ]]; then
    echo "dist.sh: missing or invalid package version in $1" >&2
    exit 1
  fi
  printf '%s\n' "$value"
}
version="$(manifest_version "$workspace/crates/covenant-compute-buyer/Cargo.toml")"
node_version="$(manifest_version "$workspace/crates/covenant-compute-node/Cargo.toml")"
stake_version="$(manifest_version "$stake_workspace/Cargo.toml")"
expected_version() {
  case "$1" in
    covenant-compute|covenant-compute-mcp) printf '%s\n' "$version" ;;
    covenant-compute-node) printf '%s\n' "$node_version" ;;
    covenant-compute-stake) printf '%s\n' "$stake_version" ;;
    *) return 1 ;;
  esac
}

if [ "$mode" = release ]; then
  for manifest in "$workspace/crates/covenant-compute-node/Cargo.toml" "$stake_workspace/Cargo.toml"; do
    packaged_version="$(manifest_version "$manifest")"
    if [ "$packaged_version" != "$version" ]; then
      echo "dist.sh: $manifest has version $packaged_version, expected $version" >&2
      exit 1
    fi
  done
  notes="$here/notes/compute-v$version.md"
  [ -f "$notes" ] || { echo "dist.sh: release notes missing: $notes" >&2; exit 1; }
  # The README links to the command reference at this release's tag.
  if grep -o -E 'compute-v[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.]+)?' "$here/README.md" | grep -q -v -x -F "compute-v$version"; then
    echo "dist.sh: README.md links to another release than compute-v$version" >&2
    exit 1
  fi
else
  source_commit="$(git -C "$root" rev-parse --verify HEAD)"
  source_tree="$(git -C "$root" rev-parse 'HEAD^{tree}')"
  source_dirty=false
  if [ -n "$(git -C "$root" status --porcelain --untracked-files=normal)" ]; then
    source_dirty=true
  fi
  asset="candidate-snapshot-${source_commit:0:12}-$asset"
fi

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
install -m 0644 "$root/LICENSE" "$root/NOTICE.md" "$stage/"
install -m 0644 "$workspace/crates/covenant-compute-buyer/skills/security-audit/upstream/LICENSE" "$stage/SECURITY_AUDIT_LICENSE"
if [ "$mode" = release ]; then
  install -m 0644 "$here/README.md" "$stage/README.md"
  install -m 0644 "$notes" "$stage/RELEASE_NOTES.md"
else
  cat > "$stage/README.md" <<EOF
# Covenant Compute candidate snapshot

This is an unreleased build for pull-request review and local qualification.
It is not a versioned release, published installer, or production activation.
The included tools may have different manifest versions. Run each tool with
\`--version\` and \`--help\` before use.

Source commit: $source_commit
Committed tree: $source_tree
Working tree had local changes: $source_dirty
Target: $target

SNAPSHOT_PROVENANCE.txt records the source identity, each expected manifest
version, and SHA-256 of each packed binary. It is informational build metadata,
not a signed release attestation. A dirty snapshot is not identified by its
commit alone. Review the PR and retain its diff when evaluating that snapshot.
EOF
  {
    printf 'schema_version=1\nartifact_kind=unreleased_candidate_snapshot\n'
    printf 'source_commit=%s\nsource_tree=%s\nsource_dirty=%s\ntarget=%s\n' "$source_commit" "$source_tree" "$source_dirty" "$target"
    for bin in "${bins[@]}" covenant-compute-stake; do
      printf '%s.manifest_version=%s\n' "$bin" "$(expected_version "$bin")"
      printf '%s.sha256=%s\n' "$bin" "$(shasum -a 256 "$stage/$bin" | awk '{print $1}')"
    done
  } > "$stage/SNAPSHOT_PROVENANCE.txt"
fi

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
  expected="$(expected_version "$bin")"
  if [ "$got" != "$bin $expected" ]; then
    echo "dist.sh: $bin --version printed '$got', expected '$bin $expected'" >&2
    exit 1
  fi
  run_packed "$path" --help >/dev/null
  if [[ $target == *-linux-musl ]] && ! file "$path" | grep -q -E 'statically linked|static-pie linked'; then
    echo "dist.sh: $bin is not statically linked: $(file "$path")" >&2
    exit 1
  fi
done

# A version string alone cannot establish that the published buyer contains the
# workspace interface. Exercise the extracted command's help before any buyer
# identity or endpoint is configured; none of these checks may create state.
buyer="$work/check/$asset/covenant-compute"
help_home="$work/help-only-buyer"
# The skill and its attribution must survive packaging and remain usable offline.
COVENANT_COMPUTE_MCP_HOME="$help_home" \
  COVENANT_COMPUTE_COORDINATOR_URL=http://127.0.0.1:1 \
  run_packed "$buyer" audit-skill --read upstream/LICENSE > "$work/audit-license"
cmp "$work/audit-license" "$work/check/$asset/SECURITY_AUDIT_LICENSE"
COVENANT_COMPUTE_MCP_HOME="$help_home" \
  COVENANT_COMPUTE_COORDINATOR_URL=http://127.0.0.1:1 \
  run_packed "$buyer" audit-skill --list > "$work/audit-documents"
grep -Fxq 'upstream/validate-findings.cjs' "$work/audit-documents"
grep -Fxq 'upstream/validate-coverage-ledger.cjs' "$work/audit-documents"
lease_help="$(COVENANT_COMPUTE_MCP_HOME="$help_home" \
  COVENANT_COMPUTE_COORDINATOR_URL=http://127.0.0.1:1 \
  run_packed "$buyer" lease open --help)"
if ! grep -Fq -- '--job-id UUID' <<< "$lease_help"; then
  echo "dist.sh: packed buyer help is missing durable explicit rental IDs" >&2
  exit 1
fi
workspace_help="$(COVENANT_COMPUTE_MCP_HOME="$help_home" \
  COVENANT_COMPUTE_COORDINATOR_URL=http://127.0.0.1:1 \
  run_packed "$buyer" workspace --help)"
for command in create 'list|status' save retry restore delete backup import; do
  if ! grep -Fq "covenant-compute workspace $command " <<< "$workspace_help"; then
    echo "dist.sh: packed buyer help is missing workspace $command" >&2
    exit 1
  fi
done
if ! grep -Fq '[--lease UUID]' <<< "$workspace_help"; then
  echo "dist.sh: packed buyer help is missing lease-bound workspace restore" >&2
  exit 1
fi
for command in create list status save retry restore delete backup import; do
  COVENANT_COMPUTE_MCP_HOME="$help_home" \
    COVENANT_COMPUTE_COORDINATOR_URL=http://127.0.0.1:1 \
    run_packed "$buyer" workspace "$command" --help >/dev/null
done
if [ -e "$help_home" ]; then
  echo "dist.sh: workspace help unexpectedly created buyer state" >&2
  exit 1
fi

mkdir -p "$root/dist"
mv "$archive" "$root/dist/"
echo "$root/dist/$asset.tar.gz"
