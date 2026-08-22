#!/usr/bin/env bash
set -euo pipefail

if [[ $# -ne 3 ]]; then
  echo "usage: $0 PROGRAM_ID RPC_A RPC_B" >&2
  exit 64
fi

program_id="$1"
rpc_a="$2"
rpc_b="$3"
program_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
artifact="$program_root/target/deploy/mizuki_escrow_program.so"
dump_a="$(mktemp -t mizuki-escrow-rpc-a.XXXXXX)"
dump_b="$(mktemp -t mizuki-escrow-rpc-b.XXXXXX)"

test -f "$artifact"
solana program show --url "$rpc_a" --commitment finalized --output json "$program_id"
solana program show --url "$rpc_b" --commitment finalized --output json "$program_id"
solana program dump --url "$rpc_a" --commitment finalized "$program_id" "$dump_a"
solana program dump --url "$rpc_b" --commitment finalized "$program_id" "$dump_b"

cmp "$dump_a" "$dump_b"
cmp "$artifact" "$dump_a"

shasum -a 256 "$artifact" "$dump_a" "$dump_b"
solana-verify get-executable-hash "$artifact"
solana-verify --url "$rpc_a" get-program-hash "$program_id"
solana-verify --url "$rpc_b" get-program-hash "$program_id"

echo "finalized dumps retained at $dump_a and $dump_b"
