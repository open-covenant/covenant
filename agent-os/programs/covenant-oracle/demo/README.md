# covenant-oracle gating demo

Mints an MPL Core agent asset on devnet with the Covenant Oracle plugin gating
its transfer, then proves the gate tracks the audit verdict:

1. `init_oracle(subject = asset)`: oracle PDA, audit valid by default
2. `create` asset + Oracle plugin: transfer gated on the PDA (offset `Anchor`)
3. `set_validation(false)`: audit invalid / out of policy
4. transfer: **rejected** by MPL Core (custom error `0x9`, which the client decodes as "Invalid Authority"), owner unchanged
5. `set_validation(true)`: audit restored
6. transfer: **succeeds**, owner moves

The rule is enforced by MPL Core; the program only flips the verdict.

## Run

```sh
pnpm install --ignore-workspace      # isolated from the covenant pnpm workspace
node gate-demo.mjs
```

Defaults to devnet RPC and `~/.config/solana/id.json` as the authority/payer.
Override with `RPC_URL` and `KEYPAIR`. The keypair needs a little devnet SOL for
the oracle account rent, the asset, and fees.

`set_validation` is sent at `finalized` so the verdict is rooted on every
load-balanced devnet RPC node before the transfer's preflight reads it; at
`confirmed` a lagging node can make the gate appear to flap.

## Wire a mainnet asset

```sh
node wire-gate.mjs <asset-pubkey>
```

`wire-gate.mjs` gates a live mainnet identity on the Oracle, then proves the
veto by simulating a transfer (the asset is never broadcast-moved) and always
restores the verdict to valid. Reads the mainnet RPC from `landing/.env.local`
and signs with `covenant-metaplex-authority.json` (DKxXr). `wire-4xtur.mjs` is a
thin wrapper that runs it for the featured 4XtUr identity.
