# covenant-inference-settlement

The settlement bridge for the inference network. It folds each served request's
receipt hash into an on-chain `provenance_root` with a gasless `consume_credits`
on a MagicBlock Ephemeral Rollup, then commits the folded root to Solana on
undelegate. The gateway posts receipts to it via `--settlement-url`.

The credit account owner is a throwaway devnet key, never the treasury. The
`serve`, `init`, and `commit` paths refuse to run against mainnet unless
`ALLOW_MAINNET=1` is set. The sanctioned mainnet path is `scripts/fold-mainnet.mjs`.

## Devnet quickstart

The devnet `cov9UDyp` deployment predates the ER instructions, so the bridge runs
against a byte-identical build deployed under a throwaway program id. Every command
targets it through `PROGRAM_ID`; without that export the CLI falls back to `cov9UDyp`,
which has no delegate/commit/undelegate and `init` will fail.

```
npm install
export SOLANA_RPC=https://api.devnet.solana.com

# 1. deploy the ephemeral settlement program to devnet. build the .so first with
#    `cargo build-sbf` in ../covenant-magicblock-er/agent-os/programs/settlement-ephemeral
PROGRAM_SO=/path/to/covenant_settlement_program_er.so node scripts/devnet-deploy.mjs

# 2. target the program you just deployed (every later command reads this)
export PROGRAM_ID=$(node -e 'console.log(require("./.keys/deploy.json").program)')

# 3. open + fund + delegate the credit account
node cli.mjs init

# 4. run the bridge
node cli.mjs serve
```

`node cli.mjs commit` and `node cli.mjs root` run the `/commit` and `/root` actions
from the command line without the server.

The full inference-stack proof (real qwen3 receipts folded through the gateway)
lives in `scripts/er-provenance-e2e.sh`. It is a bash script and needs more than
this repo: the gateway in `../agent-os/crates/covenant-inference-gateway`, plus
`ollama` (qwen3:8b) and `llama-server` on the host.

```
bash scripts/er-provenance-e2e.sh
```

## HTTP surface (`cli.mjs serve`, default `127.0.0.1:8799`)

- `POST /fold {receipt_hash_hex, amount}` folds one receipt via `consume_credits`
  on the ER, returns `{er_sig, provenance_root_hex, position}`. Folds run one at a
  time in receipt order; the chain is `root = sha256(root || receipt_hash)`.
- `POST /commit` commits and undelegates, reconciling the folded root to L1.
  Returns 500 if it does not reconcile.
- `GET /root` current ER and L1 roots.
- `GET /health` owner, credit account, and ER endpoint.

## Key env vars

- `PROGRAM_ID` settlement program to target (default `cov9UDyp`; on devnet set it to the deploy in `.keys/deploy.json`).
- `SOLANA_RPC` L1 RPC (default devnet).
- `ER_URL` / `VALIDATOR` the MagicBlock ER endpoint and pinned validator.
- `KEYPAIR` owner keypair (default `./.keys/owner.json`, gitignored).
- `SETTLEMENT_PORT` serve port (default 8799).
- `TARGET_CREDITS` credits to hold after `init` (default 50000).
- `ALLOW_MAINNET=1` override the mainnet refusal (only for a deliberate mainnet run).

## Mainnet

`scripts/fold-mainnet.mjs` is the one-shot mainnet fold: it delegates a credit
account, folds a captured set of receipt hashes, then commits and undelegates,
with a genesis check before every signing step. `scripts/recover-undelegate.mjs`
brings a stranded account home. Both are owner-signed and owner-paid on the ER.
</content>
