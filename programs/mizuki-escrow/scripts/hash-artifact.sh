#!/usr/bin/env bash
set -euo pipefail

program_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
artifact="$program_root/target/deploy/mizuki_escrow_program.so"

test -f "$artifact"
shasum -a 256 "$artifact"
solana-verify get-executable-hash "$artifact"
