#!/usr/bin/env bash
#
# Smoke test: `npx skills add <repo-root> --list` discovers the covenant skill
# and reports the frontmatter description.

set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
root="$(cd "${here}/.." && pwd)"

out="$(npx --yes skills@1 add "${root}" --list 2>&1)"

if ! grep -q "covenant" <<<"${out}"; then
  echo "tests/list.sh: skill name 'covenant' not found in output:" >&2
  echo "${out}" >&2
  exit 1
fi

echo "ok"
