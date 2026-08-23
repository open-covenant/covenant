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
work_dir="$(mktemp -d -t mizuki-escrow-verify.XXXXXX)"
metadata_a="$work_dir/rpc-a-program.json"
metadata_b="$work_dir/rpc-b-program.json"
normalized_a="$work_dir/rpc-a-program.normalized.json"
normalized_b="$work_dir/rpc-b-program.normalized.json"
dump_a="$work_dir/rpc-a-program.so"
dump_b="$work_dir/rpc-b-program.so"
upgradeable_loader='BPFLoaderUpgradeab1e11111111111111111111111'

command -v jq >/dev/null
command -v solana >/dev/null
command -v solana-verify >/dev/null

validate_metadata() {
  local metadata="$1"
  jq -e \
    --arg program_id "$program_id" \
    --arg loader "$upgradeable_loader" \
    '.programId == $program_id and
      .owner == $loader and
      .authority == null and
      (.programdataAddress | type) == "string" and
      (.programdataAddress | length) > 0 and
      (.dataLen | type) == "number" and
      .dataLen > 0' \
    "$metadata" >/dev/null
}

read_executable_hash() {
  local output hash
  output="$("$@")"
  hash="$(printf '%s\n' "$output" | awk 'NF { value = $NF } END { print value }')"
  if [[ ! "$hash" =~ ^([0-9a-f]{64}|[1-9A-HJ-NP-Za-km-z]{32,64})$ ]]; then
    echo "could not parse executable hash" >&2
    return 1
  fi
  printf '%s' "$hash"
}

test -f "$artifact"
elf_flags="$(od -An -tx1 -j 48 -N 4 "$artifact" | tr -d '[:space:]')"
if [[ "$elf_flags" != "02000000" ]]; then
  echo "expected SBPFv2 ELF flags (0x2), found 0x$elf_flags" >&2
  exit 1
fi
solana program show --url "$rpc_a" --commitment finalized --output json "$program_id" >"$metadata_a"
solana program show --url "$rpc_b" --commitment finalized --output json "$program_id" >"$metadata_b"
validate_metadata "$metadata_a"
validate_metadata "$metadata_b"
jq -S '{programId, owner, programdataAddress, authority, dataLen}' "$metadata_a" >"$normalized_a"
jq -S '{programId, owner, programdataAddress, authority, dataLen}' "$metadata_b" >"$normalized_b"
cmp "$normalized_a" "$normalized_b"

solana program dump --url "$rpc_a" --commitment finalized "$program_id" "$dump_a"
solana program dump --url "$rpc_b" --commitment finalized "$program_id" "$dump_b"

cmp "$dump_a" "$dump_b"
cmp "$artifact" "$dump_a"

local_sha256="$(shasum -a 256 "$artifact" | awk '{print $1}')"
rpc_a_sha256="$(shasum -a 256 "$dump_a" | awk '{print $1}')"
rpc_b_sha256="$(shasum -a 256 "$dump_b" | awk '{print $1}')"
if [[ "$local_sha256" != "$rpc_a_sha256" || "$local_sha256" != "$rpc_b_sha256" ]]; then
  echo "raw program SHA-256 mismatch" >&2
  exit 1
fi

local_executable_hash="$(read_executable_hash solana-verify get-executable-hash "$artifact")"
rpc_a_executable_hash="$(read_executable_hash solana-verify --url "$rpc_a" get-program-hash "$program_id")"
rpc_b_executable_hash="$(read_executable_hash solana-verify --url "$rpc_b" get-program-hash "$program_id")"
if [[ "$local_executable_hash" != "$rpc_a_executable_hash" ||
  "$local_executable_hash" != "$rpc_b_executable_hash" ]]; then
  echo "Solana executable hash mismatch" >&2
  exit 1
fi

printf 'SHA-256: %s\n' "$local_sha256"
printf 'Solana executable hash: %s\n' "$local_executable_hash"

echo "finalized verification evidence retained at $work_dir"
