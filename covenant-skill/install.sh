#!/usr/bin/env bash
#
# Install the `covenant` skill into a target agent project or directory.
#
# Usage:
#   ./install.sh --project <path>    Install into <path>/.claude/skills/covenant
#   ./install.sh --path <path>       Install into <path>/covenant
#   ./install.sh                     Install into ./.claude/skills/covenant
#
# Mirrors the convention used by solana-foundation/solana-dev-skill: the script
# never executes remote bootstrap (no curl|sh, no npx ...@latest), never requests
# secrets, and copies the local `skill/` directory tree into the destination.

set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
src="${here}/skill"

if [[ ! -d "${src}" ]]; then
  echo "install.sh: missing source directory: ${src}" >&2
  exit 1
fi

dest=""
mode=""

while [[ $# -gt 0 ]]; do
  case "$1" in
    --project)
      [[ $# -ge 2 ]] || { echo "install.sh: --project requires a path" >&2; exit 2; }
      mode="project"
      dest="$2/.claude/skills/covenant"
      shift 2
      ;;
    --path)
      [[ $# -ge 2 ]] || { echo "install.sh: --path requires a path" >&2; exit 2; }
      mode="path"
      dest="$2/covenant"
      shift 2
      ;;
    -h|--help)
      sed -n '2,12p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'
      exit 0
      ;;
    *)
      echo "install.sh: unknown argument: $1" >&2
      exit 2
      ;;
  esac
done

if [[ -z "${dest}" ]]; then
  mode="project"
  dest="$(pwd)/.claude/skills/covenant"
fi

mkdir -p "${dest}"
cp -R "${src}/." "${dest}/"

echo "installed covenant skill (${mode}): ${dest}"
