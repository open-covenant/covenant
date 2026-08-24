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
repo_root="$(cd "$program_root/../.." && pwd)"
artifact="$program_root/target/deploy/mizuki_escrow_program.so"
work_dir="$(mktemp -d -t mizuki-escrow-verify.XXXXXX)"
metadata_a="$work_dir/rpc-a-program.json"
metadata_b="$work_dir/rpc-b-program.json"
normalized_a="$work_dir/rpc-a-program.normalized.json"
normalized_b="$work_dir/rpc-b-program.normalized.json"
dump_a="$work_dir/rpc-a-program.so"
dump_b="$work_dir/rpc-b-program.so"
provider_evidence="$work_dir/rpc-provider-domains.json"
genesis_evidence="$work_dir/rpc-genesis-hashes.json"
upgradeable_loader='BPFLoaderUpgradeab1e11111111111111111111111'
mainnet_genesis_hash='5eykt4UsFv8P8NJdTREpY1vzqKqZKvdpKuc147dw2N9d'

command -v jq >/dev/null
command -v node >/dev/null
command -v curl >/dev/null
command -v solana >/dev/null
command -v solana-verify >/dev/null

assert_independent_rpc_providers() {
  (
    cd "$repo_root/services/mizuki-policy-signer"
    node --input-type=module - "$rpc_a" "$rpc_b" <<'NODE'
import { isIP } from 'node:net';
import { getDomain } from 'tldts';

function fail(message) {
  process.stderr.write(`${message}\n`);
  process.exit(1);
}

let endpoints;
try {
  endpoints = process.argv.slice(2).map((value) => new URL(value));
} catch {
  fail('mainnet RPC endpoints must be valid absolute URLs');
}

function host(url) {
  return url.hostname.toLowerCase().replace(/\.$/, '').replace(/^\[|\]$/g, '');
}

function endpointIdentity(url) {
  const port = url.port || (url.protocol === 'https:' ? '443' : '');
  const path = url.pathname.replace(/\/+$/, '') || '/';
  return `${url.protocol}//${host(url)}:${port}${path}`;
}

function providerDomain(url) {
  const hostname = host(url);
  if (isIP(hostname)) fail('mainnet RPC providers must use DNS hostnames');
  const domain = getDomain(hostname, { allowPrivateDomains: false });
  if (!domain) fail('mainnet RPC providers must use registrable DNS domains');
  return domain;
}

if (endpoints.some((url) => url.protocol !== 'https:')) {
  fail('mainnet RPC providers must use HTTPS');
}
if (endpointIdentity(endpoints[0]) === endpointIdentity(endpoints[1])) {
  fail('mainnet RPC endpoints must be different');
}
if (providerDomain(endpoints[0]) === providerDomain(endpoints[1])) {
  fail('mainnet RPC providers must use different domains');
}
process.stdout.write(`${JSON.stringify({
  schema: 'mizuki.rpc-provider-evidence.v1',
  primary: providerDomain(endpoints[0]),
  secondary: providerDomain(endpoints[1]),
  redirects: 'forbidden',
})}\n`);
NODE
  )
}

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

read_genesis_hash() {
  local rpc="$1" response hash
  response="$(curl \
    --silent \
    --show-error \
    --fail-with-body \
    --location \
    --max-redirs 0 \
    --proto '=https' \
    --proto-redir '=https' \
    --connect-timeout 5 \
    --max-time 10 \
    --header 'content-type: application/json' \
    --data-binary '{"jsonrpc":"2.0","id":1,"method":"getGenesisHash"}' \
    "$rpc")"
  hash="$(jq -er '.result | select(type == "string")' <<<"$response")"
  if [[ ! "$hash" =~ ^[1-9A-HJ-NP-Za-km-z]{32,64}$ ]]; then
    echo "could not parse genesis hash" >&2
    return 1
  fi
  printf '%s' "$hash"
}

assert_independent_rpc_providers >"$provider_evidence"
"$program_root/scripts/validate-artifact.sh" "$artifact"
rpc_a_genesis_hash="$(read_genesis_hash "$rpc_a")"
rpc_b_genesis_hash="$(read_genesis_hash "$rpc_b")"
if [[ "$rpc_a_genesis_hash" != "$mainnet_genesis_hash" ||
  "$rpc_b_genesis_hash" != "$mainnet_genesis_hash" ]]; then
  echo "both RPC providers must report the canonical Solana mainnet-beta genesis hash" >&2
  exit 1
fi
jq -n \
  --arg schema 'mizuki.rpc-genesis-evidence.v1' \
  --arg cluster 'mainnet-beta' \
  --arg expected "$mainnet_genesis_hash" \
  --arg primary "$rpc_a_genesis_hash" \
  --arg secondary "$rpc_b_genesis_hash" \
  '{
    schema: $schema,
    cluster: $cluster,
    expected: $expected,
    observations: { primary: $primary, secondary: $secondary }
  }' >"$genesis_evidence"
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
printf 'Mainnet genesis hash: %s\n' "$mainnet_genesis_hash"

echo "finalized verification evidence retained at $work_dir"
