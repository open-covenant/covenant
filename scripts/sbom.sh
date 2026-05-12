#!/usr/bin/env bash
# Generate an SPDX SBOM for the Covenant Rust workspace using cargo-sbom.
# Output: dist/covenant-sbom.spdx.json
#
# Install dependency if missing:
#   cargo install cargo-sbom

set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
OUT_DIR="$ROOT/dist"
OUT_FILE="$OUT_DIR/covenant-sbom.spdx.json"

if ! command -v cargo-sbom >/dev/null 2>&1; then
  echo "cargo-sbom not found. Install with: cargo install cargo-sbom" >&2
  exit 1
fi

mkdir -p "$OUT_DIR"

(
  cd "$ROOT/agent-os"
  cargo sbom --output-format spdx_json_2_3
) > "$OUT_FILE"

echo "wrote $OUT_FILE ($(wc -c < "$OUT_FILE") bytes)"
