#!/usr/bin/env bash
# Run cargo-audit across every Cargo.lock in the repo, applying the
# shared baseline in ./audit.toml. Exits non-zero on any advisory
# not in the baseline ignore list.
#
# Usage: scripts/audit.sh          # scans agent-os + services/indexer
#        scripts/audit.sh <file>   # scans a single lockfile
set -euo pipefail

cd "$(dirname "$0")/.."

BASELINE="audit.toml"
if [[ ! -f "$BASELINE" ]]; then
  echo "audit.sh: $BASELINE missing; refusing to run with no policy" >&2
  exit 2
fi

# Extract advisory IDs from the baseline. Accepts lines of the form
#   "RUSTSEC-YYYY-NNNN",
# inside the `ignore = [...]` array. Comments and blanks ignored.
# Uses a while-read loop (works on bash 3.2, macOS default).
IGNORE_FLAGS=()
while IFS= read -r id; do
  [[ -z "$id" ]] && continue
  IGNORE_FLAGS+=(--ignore "$id")
done < <(
  awk '
    /^\[advisories\]/ { in_adv = 1; next }
    /^\[/             { in_adv = 0 }
    in_adv && /^[[:space:]]*"RUSTSEC-[0-9]+-[0-9]+"/ {
      gsub(/[",]/, "")
      gsub(/^[[:space:]]+/, "")
      print
    }
  ' "$BASELINE"
)

run_audit() {
  local lockfile="$1"
  echo ">> cargo audit --deny warnings --file $lockfile"
  if [[ "${#IGNORE_FLAGS[@]}" -eq 0 ]]; then
    cargo audit --deny warnings --file "$lockfile"
  else
    cargo audit --deny warnings --file "$lockfile" "${IGNORE_FLAGS[@]}"
  fi
}

if [[ $# -gt 0 ]]; then
  run_audit "$1"
  exit 0
fi

LOCKFILES=(
  "agent-os/Cargo.lock"
  "services/indexer/Cargo.lock"
)

FAIL=0
for lock in "${LOCKFILES[@]}"; do
  if [[ ! -f "$lock" ]]; then
    echo ">> skipping $lock (missing)"
    continue
  fi
  if ! run_audit "$lock"; then
    FAIL=1
  fi
done

exit "$FAIL"
