# covenant-payai

A trust layer over the [PayAI](https://payai.network) x402 payment rail. It reads
PayAI's public on-chain settlements and derives verifiable reputation and work
receipts from them, without touching the payment flow.

PayAI settles agent-to-agent payments on-chain (a real USDC transfer, gas paid by
the facilitator). It says nothing about whether the work was delivered and keeps no
reputation. This crate adds those two pieces:

- **Settlement-grounded reputation**: per-wallet reputation (settled jobs, distinct
  counterparties, volume, tier, 0..1000 score) computed from real on-chain USDC
  settlements and signed with the Covenant identity (ed25519). The output is a
  credential an agent can present.
- **Signed work-receipts**: a seller binds an on-chain settlement to what was
  delivered (settlement tx, output hash, seller id) and signs it. The buyer can
  counter-sign to accept.

Read-after-settlement and attest only. No `solana-sdk`: Solana is read over plain
JSON-RPC (`reqwest`), so the crate stays light and daemon-linkable.

## Layout

| File            | Responsibility                                                      |
| --------------- | ------------------------------------------------------------------ |
| `index.rs`      | Settlement indexer: watch the PayAI fee-payer, parse USDC `TransferChecked`, resolve owner wallets. |
| `reputation.rs` | Settlement-grounded scoring + tiers.                               |
| `receipt.rs`    | `WorkReceipt` issue / verify / counter-sign.                       |
| `sign.rs`       | ed25519 signed-credential envelope (reuses `covenant-identity`).   |
| `oracle.rs`     | `signed_reputation_for(...)`: fetch, compute, sign in one call.    |
| `tool.rs`       | The `payai.reputation` MCP tool + specs.                          |
| `config.rs`     | `PayAiConfig`, env-driven daemon config.                          |
| `types.rs`      | `Settlement`, `Tier`, `PayaiReputation`.                          |

## Try it

Unit tests (no network):

```sh
cargo test -p covenant-payai
```

Live reputation snapshot against PayAI's real mainnet settlements (network):

```sh
cargo run -p covenant-payai --example scan
# override RPC / window:  RPC=<url> LIMIT=<n> cargo run -p covenant-payai --example scan
# default RPC = https://solana-rpc.publicnode.com (api.mainnet-beta throttles getTransaction)
```

The live indexer test is ignored by default (it hits the network):

```sh
cargo test -p covenant-payai -- --ignored
```

## In the daemon

`covenantd` exposes a `payai.reputation` MCP tool when enabled:

```sh
COVENANT_PAYAI_ENABLED=1
COVENANT_PAYAI_RPC_URL=<solana rpc>   # falls back to COVENANT_SOLANA_RPC_URL
COVENANT_PAYAI_LIMIT=<recent sigs>    # default 50
```

Call it for a wallet to get the reputation plus its signed credential inline. The
call is capability-gated by `tool.call.payai.reputation`.

## On "full on-chain settlement"

PayAI settles the *payment* on-chain (a real USDC transfer, facilitator-paid gas),
which is why reputation here is grounded in something verifiable. It is not on-chain
*escrow* (settlement is a direct transfer), the facilitator is off-chain and
centralized (one fee-payer co-signs every settlement), and only the money is
on-chain. The job and its delivery are not. This crate never moves funds.
