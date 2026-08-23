#!/usr/bin/env bash
set -euo pipefail

program_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
base_image="solanafoundation/solana-verifiable-build:4.0.0@sha256:0b4e3716fad9ca4b4aac3e3f977f43aad93a18c22296c0c0f44fc22e644bdd68"

command -v docker >/dev/null
command -v solana-verify >/dev/null

solana-verify build "$program_root" \
  --library-name mizuki_escrow_program \
  --arch v2 \
  --base-image "$base_image" \
  -- \
  --locked
