# covenant-oracle gating demo

Mints an MPL Core agent asset on devnet with the Covenant Oracle plugin gating
its transfer, then proves the gate tracks the audit verdict:

1. `init_oracle(subject = asset)` — oracle PDA, audit valid by default
2. `create` asset + Oracle plugin — transfer gated on the PDA (offset `Anchor`)
3. `set_validation(false)` — audit invalid / out of policy
4. transfer — **rejected** by MPL Core (`0x9`), owner unchanged
5. `set_validation(true)` — audit restored
6. transfer — **succeeds**, owner moves

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
