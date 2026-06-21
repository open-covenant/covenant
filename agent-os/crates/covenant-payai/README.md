# covenant-payai

Covenant's trust layer for the [PayAI](https://payai.network) x402 payment rail.
It reads PayAI's public on-chain settlements and turns them into verifiable
trust primitives, **without ever touching the payment flow**.

PayAI settles agent-to-agent payments on-chain (a real USDC transfer, gas paid
by the facilitator) but says nothing about whether the work was delivered, and
keeps no reputation. This crate fills that half:

- **Settlement-grounded reputation** — per-wallet reputation (settled jobs,
  distinct counterparties, volume, tier, 0..1000 score) computed from real
  on-chain USDC settlements and signed with the Covenant identity (ed25519). The
  result is a credential an agent can present, not a number on someone's page.
- **Signed work-receipts** — a seller binds an on-chain settlement to what was
  delivered (settlement tx, output hash, seller id) and signs it; the buyer can
  counter-sign to accept. Closes x402's "irreversible payment, no delivery
  proof" gap.

Read-after-settlement and attest only. No `solana-sdk`: Solana is read over
plain JSON-RPC (`reqwest`), so the crate stays light and daemon-linkable.

## Layout

| File            | Responsibility                                                      |
| --------------- | ------------------------------------------------------------------ |
| `index.rs`      | Settlement indexer: watch the PayAI fee-payer, parse USDC `TransferChecked`, resolve owner wallets. |
| `reputation.rs` | Settlement-grounded scoring + tiers.                               |
| `receipt.rs`    | `WorkReceipt` issue / verify / counter-sign.                       |
| `sign.rs`       | ed25519 signed-credential envelope (reuses `covenant-identity`).   |
| `oracle.rs`     | `signed_reputation_for(...)`: fetch → compute → sign, one call.    |
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

Call it for a wallet and you get the reputation plus its signed credential
inline. The call is capability-gated by `tool.call.payai.reputation`.

## On "full on-chain settlement"

PayAI settles the *payment* on-chain (a real USDC transfer, facilitator-paid
gas), which is exactly why reputation here is grounded in something verifiable.
It is not on-chain *escrow* (settlement is a direct transfer), the facilitator
itself is off-chain and centralized (one fee-payer co-signs every settlement),
and only the money is on-chain — the job and its delivery are not. This crate
adds the missing trust half alongside that rail; it never moves funds.
