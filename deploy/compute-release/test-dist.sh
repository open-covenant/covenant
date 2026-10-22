#!/usr/bin/env bash
# Hermetic packager checks. Cargo is stubbed; the actual dist.sh still creates,
# extracts and executes its archive. These tests do not claim Rust compilation.
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
fixture="$work/repo"
tools="$work/tools"
mkdir -p "$fixture/deploy/compute-release/notes" "$tools"
cp "$here/dist.sh" "$fixture/deploy/compute-release/"
for package in covenant-compute-buyer covenant-compute-node covenant-compute-lease-signer; do
  mkdir -p "$fixture/agent-os/crates/$package"
  version=0.4.1
  [ "$package" != covenant-compute-node ] || version=0.5.0
  printf '[package]\nname = "%s"\nversion = "%s"\n' "$package" "$version" > "$fixture/agent-os/crates/$package/Cargo.toml"
done
printf '[workspace]\n' > "$fixture/agent-os/Cargo.toml"
printf 'fixture license\n' > "$fixture/LICENSE"
printf 'fixture notice\n' > "$fixture/NOTICE.md"
printf 'release compute-v0.4.1\n' > "$fixture/deploy/compute-release/README.md"
printf 'release notes\n' > "$fixture/deploy/compute-release/notes/compute-v0.4.1.md"
license_dir="$fixture/agent-os/crates/covenant-compute-buyer/skills/security-audit/upstream"
mkdir -p "$license_dir"
printf 'upstream fixture license\n' > "$license_dir/LICENSE"
printf '/dist/\n/agent-os/target/\n/agent-os/crates/covenant-compute-lease-signer/target/\n' > "$fixture/.gitignore"
git -C "$fixture" init -q
git -C "$fixture" add .
git -C "$fixture" -c user.name='Packaging test' -c user.email='packaging@example.invalid' -c commit.gpgsign=false -c core.hooksPath=/dev/null commit -qm fixture
source_commit="$(git -C "$fixture" rev-parse HEAD)"

cat > "$tools/cargo" <<'CARGO'
#!/usr/bin/env bash
set -euo pipefail
command="$1"; shift
if [ "$command" = build ]; then
  case " $* " in *' --locked '*) ;; *) exit 4 ;; esac
  case " $* " in *' --release '*) ;; *) exit 4 ;; esac
fi
manifest= target= bins=()
while [ "$#" -gt 0 ]; do
  case "$1" in
    --manifest-path) manifest="$2"; shift 2 ;;
    --target) target="$2"; shift 2 ;;
    --bin) bins+=("$2"); shift 2 ;;
    *) shift ;;
  esac
done
workspace="$(dirname "$manifest")"
if [ "$command" = metadata ]; then
  printf '{"target_directory":"%s/target"}\n' "$workspace"
  exit
fi
[ "$command" = build ] || exit 2
printf 'build %s\n' "$manifest" >> "$SNAPSHOT_TEST_LOG"
mkdir -p "$workspace/target/$target/release"
for bin in "${bins[@]}"; do
  case "$bin" in
    covenant-compute|covenant-compute-mcp) package=covenant-compute-buyer ;;
    covenant-compute-node) package=covenant-compute-node ;;
    covenant-compute-stake) package=covenant-compute-lease-signer ;;
    *) exit 2 ;;
  esac
  version="$(sed -n 's/^version = "\(.*\)"$/\1/p' "$SNAPSHOT_TEST_ROOT/agent-os/crates/$package/Cargo.toml")"
  if [ "$bin" = covenant-compute-node ] && [ -n "${SNAPSHOT_TEST_BAD_NODE_VERSION:-}" ]; then
    version="$SNAPSHOT_TEST_BAD_NODE_VERSION"
  fi
  path="$workspace/target/$target/release/$bin"
  printf '#!/usr/bin/env bash\nset -euo pipefail\nbin=%s\nversion=%s\n' "$bin" "$version" > "$path"
  cat >> "$path" <<'BINARY'
case "$*" in
  --version) printf '%s %s\n' "$bin" "$version" ;;
  --help) printf 'fixture help\n' ;;
  'audit-skill --read upstream/LICENSE') cat "$SNAPSHOT_TEST_ROOT/agent-os/crates/covenant-compute-buyer/skills/security-audit/upstream/LICENSE" ;;
  'audit-skill --list') printf 'upstream/validate-findings.cjs\nupstream/validate-coverage-ledger.cjs\n' ;;
  'lease open --help') printf '%s\n' '--job-id UUID' ;;
  'workspace --help')
    for command in create 'list|status' save retry restore delete backup import; do
      printf 'covenant-compute workspace %s [--lease UUID]\n' "$command"
    done ;;
  workspace*--help) : ;;
  *) exit 3 ;;
esac
BINARY
  chmod +x "$path"
done
CARGO
chmod +x "$tools/cargo"

run_dist() {
  env PATH="$tools:$PATH" SNAPSHOT_TEST_ROOT="$fixture" SNAPSHOT_TEST_LOG="$work/builds" \
    GITHUB_REF="${SNAPSHOT_TEST_REF:-refs/heads/fixture}" \
    GITHUB_REF_TYPE="${SNAPSHOT_TEST_REF_TYPE:-branch}" \
    bash "$fixture/deploy/compute-release/dist.sh" "$@" > "$work/output" 2>&1
}
expect_success() {
  if ! run_dist "$@"; then
    cat "$work/output" >&2
    exit 1
  fi
}
reset_run() {
  rm -rf "$fixture/dist" "$work/unpacked"
  : > "$work/builds"
}
expect_failure() {
  local message="$1"; shift
  if run_dist "$@"; then
    echo "test-dist: expected rejection: $message" >&2; exit 1
  fi
  grep -Fq "$message" "$work/output"
  [ ! -d "$fixture/dist" ]
}
expect_builds() { [ "$(wc -l < "$work/builds" | tr -d ' ')" = "$1" ]; }
unpack() {
  mkdir "$work/unpacked"
  tar -C "$work/unpacked" -xzf "$fixture/dist/$1.tar.gz"
}
target=aarch64-apple-darwin
snapshot="candidate-snapshot-${source_commit:0:12}-covenant-compute-darwin-arm64"
release=covenant-compute-darwin-arm64

reset_run
expect_failure 'has version 0.5.0, expected 0.4.1' "$target"
expect_builds 0

reset_run
expect_success --snapshot "$target"
expect_builds 2
unpack "$snapshot"
packed="$work/unpacked/$snapshot"
grep -Fxq 'artifact_kind=unreleased_candidate_snapshot' "$packed/SNAPSHOT_PROVENANCE.txt"
grep -Fxq "source_commit=$source_commit" "$packed/SNAPSHOT_PROVENANCE.txt"
grep -Fxq 'source_dirty=false' "$packed/SNAPSHOT_PROVENANCE.txt"
grep -Fxq 'covenant-compute.manifest_version=0.4.1' "$packed/SNAPSHOT_PROVENANCE.txt"
grep -Fxq 'covenant-compute-node.manifest_version=0.5.0' "$packed/SNAPSHOT_PROVENANCE.txt"
grep -Fq 'unreleased build' "$packed/README.md"
[ ! -f "$packed/RELEASE_NOTES.md" ]
for bin in covenant-compute covenant-compute-mcp covenant-compute-node covenant-compute-stake; do
  digest="$(shasum -a 256 "$packed/$bin" | awk '{print $1}')"
  grep -Fxq "$bin.sha256=$digest" "$packed/SNAPSHOT_PROVENANCE.txt"
done

reset_run
SNAPSHOT_TEST_BAD_NODE_VERSION=0.4.1 expect_failure "expected 'covenant-compute-node 0.5.0'" --snapshot "$target"
expect_builds 2

for tag_context in ref type; do
  reset_run
  if [ "$tag_context" = ref ]; then
    SNAPSHOT_TEST_REF=refs/tags/compute-v0.4.1 expect_failure 'snapshot mode is forbidden for tag refs' --snapshot "$target"
  else
    SNAPSHOT_TEST_REF_TYPE=tag expect_failure 'snapshot mode is forbidden for tag refs' --snapshot "$target"
  fi
  expect_builds 0
done

sed 's/version = "0.5.0"/version = "0.4.1"/' "$fixture/agent-os/crates/covenant-compute-node/Cargo.toml" > "$work/node.toml"
mv "$work/node.toml" "$fixture/agent-os/crates/covenant-compute-node/Cargo.toml"
reset_run
expect_success "$target"
expect_builds 2
unpack "$release"
[ -f "$work/unpacked/$release/RELEASE_NOTES.md" ]
[ ! -f "$work/unpacked/$release/SNAPSHOT_PROVENANCE.txt" ]
cmp "$fixture/deploy/compute-release/README.md" "$work/unpacked/$release/README.md"

reset_run
SNAPSHOT_TEST_BAD_NODE_VERSION=9.9.9 expect_failure "expected 'covenant-compute-node 0.4.1'" "$target"
expect_builds 2

rm "$fixture/deploy/compute-release/notes/compute-v0.4.1.md"
reset_run
expect_failure 'release notes missing' "$target"
expect_builds 0
printf 'release notes\n' > "$fixture/deploy/compute-release/notes/compute-v0.4.1.md"
printf 'wrong release compute-v9.9.9\n' > "$fixture/deploy/compute-release/README.md"
reset_run
expect_failure 'README.md links to another release' "$target"
expect_builds 0

# Snapshot metadata cannot accidentally claim stale release notes or README text.
rm "$fixture/deploy/compute-release/notes/compute-v0.4.1.md"
reset_run
expect_success --snapshot "$target"
expect_builds 2
unpack "$snapshot"
grep -Fxq 'source_dirty=true' "$work/unpacked/$snapshot/SNAPSHOT_PROVENANCE.txt"
if grep -Fq compute-v9.9.9 "$work/unpacked/$snapshot/README.md"; then exit 1; fi
[ ! -f "$work/unpacked/$snapshot/RELEASE_NOTES.md" ]
printf 'test-dist: 10 release/snapshot packaging checks passed (stub Cargo; real archive checks)\n'
