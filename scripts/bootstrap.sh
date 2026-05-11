#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "$0")/.."

RED='\033[0;31m'
GREEN='\033[0;32m'
NC='\033[0m'

ok()   { printf "${GREEN}✓${NC} %s\n" "$1"; }
fail() { printf "${RED}✗${NC} %s\n" "$1"; exit 1; }

command -v rustc >/dev/null 2>&1 || fail "rustc not found — install via https://rustup.rs"
command -v node >/dev/null 2>&1 || fail "node not found — install Node 22+ via https://nodejs.org"
command -v pnpm >/dev/null 2>&1 || fail "pnpm not found — install via corepack"

NODE_MAJOR="$(node -e "console.log(process.versions.node.split('.')[0])")"
[ "$NODE_MAJOR" -ge 22 ] || fail "Node 22+ required (found v$(node --version))"

ok "rust:  $(rustc --version)"
ok "node:  $(node --version)"
ok "pnpm:  $(pnpm --version)"

if command -v anchor >/dev/null 2>&1; then
  ok "anchor: $(anchor --version)"
else
  ok "anchor: not installed; skip Solana program build until Anchor is available"
fi

if command -v solana >/dev/null 2>&1; then
  ok "solana: $(solana --version)"
else
  ok "solana: not installed; install the Solana CLI before local validator work"
fi

if [ ! -f .env ]; then
  cp .env.example .env
  ok ".env created from .env.example"
else
  ok ".env already exists"
fi

echo
echo ">> installing dependencies..."
pnpm install --frozen-lockfile

echo
echo ">> typechecking the workspace..."
pnpm typecheck

if command -v anchor >/dev/null 2>&1; then
  echo
  echo ">> building Solana protocol program..."
  (cd agent-os && anchor build)
fi

echo
echo ">> running quick daemon validation..."
bash agent-os/scripts/validate.sh --quick

echo
printf "\n${GREEN}bootstrap complete.${NC}\n\n"
echo "next steps:"
echo "  docker compose up -d                   # start postgres + redis"
echo "  solana-test-validator                  # local Solana validator"
echo "  cd agent-os && anchor test             # program integration tests"
echo "  bash agent-os/scripts/validate.sh      # full daemon validation"
echo "  pnpm test                              # run workspace tests"
echo
