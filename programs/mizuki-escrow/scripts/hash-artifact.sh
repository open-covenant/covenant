#!/usr/bin/env bash
set -euo pipefail

program_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
artifact="$program_root/target/deploy/mizuki_escrow_program.so"

test -f "$artifact"
elf_flags="$(od -An -tx1 -j 48 -N 4 "$artifact" | tr -d '[:space:]')"
if [[ "$elf_flags" != "02000000" ]]; then
  echo "expected SBPFv2 ELF flags (0x2), found 0x$elf_flags" >&2
  exit 1
fi
shasum -a 256 "$artifact"
solana-verify get-executable-hash "$artifact"
