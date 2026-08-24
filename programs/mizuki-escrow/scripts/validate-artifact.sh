#!/usr/bin/env bash
set -euo pipefail

if [[ $# -ne 1 ]]; then
  echo "usage: $0 ARTIFACT" >&2
  exit 64
fi

artifact="$1"
test -f "$artifact"

elf_machine="$(od -An -tu2 -j 18 -N 2 "$artifact" | tr -d '[:space:]')"
if [[ "$elf_machine" != "263" ]]; then
  echo "expected EM_SBF ELF machine 263, found $elf_machine" >&2
  exit 1
fi

elf_flags="$(od -An -tx1 -j 48 -N 4 "$artifact" | tr -d '[:space:]')"
if [[ "$elf_flags" != "02000000" ]]; then
  echo "expected SBPFv2 ELF flags (0x2), found 0x$elf_flags" >&2
  exit 1
fi
