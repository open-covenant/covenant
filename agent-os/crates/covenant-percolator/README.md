# Permissionless Accountable Keepers for Percolator

Covenant-governed keepers for Anatoly Yakovenko's [Percolator v16](https://github.com/aeyakovenko/percolator-prog) perpetuals protocol on Solana. Capability-scoped, budget-bounded, audit-logged, slashably-bonded.

**TL;DR.** Percolator pushes liveness to permissionless cranks. This crate is the accountability layer that lets a swarm of independent operators do that work safely — each keeper carries an operator-signed scope and a SOL bond. A scope violation costs lamports.

## Live artifacts

| Layer | Identifier | Network | Cluster |
| --- | --- | --- | --- |
| Percolator v16 program (read target) | [`4m3ipBQDYX6JQ9YSmUXDjESDHMtGWtiXforkWr9Qoxdi`](https://explorer.solana.com/address/4m3ipBQDYX6JQ9YSmUXDjESDHMtGWtiXforkWr9Qoxdi) | mainnet-beta | Bounty 6 |
| Bounty 6 market group | [`BhkMic5gHLjj5Uxkg6rBBXofUzeTZVwmV4uFzfhwtgQw`](https://explorer.solana.com/address/BhkMic5gHLjj5Uxkg6rBBXofUzeTZVwmV4uFzfhwtgQw) | mainnet-beta | USD/SOL · STOXX50/SOL · BTC/SOL at 20× |
| **Covenant Bond program (this crate)** | [`DMy5XmGmYbBzvtRefRyqJTwBwFvo2WHEwrC3fgfLtEGE`](https://explorer.solana.com/address/DMy5XmGmYbBzvtRefRyqJTwBwFvo2WHEwrC3fgfLtEGE?cluster=devnet) | **devnet** | upgradeable, 162744 bytes |

The Bond program is deployed and end-to-end verified on devnet via [`examples/devnet_smoke.rs`](../covenant-percolator-bond-program/examples/devnet_smoke.rs). Sample signatures: `5akxKk7e…` (init), `5SbTSb6d…` (deposit 0.05 SOL), `EFRs23V2…` (slash drains bond, recipient credited).

## What's here

This is a worktree organized into three crates + a paper:

```
agent-os/crates/
├── covenant-percolator/          ← decision + governance layer
│   ├── src/capability.rs         · operator-signed KeeperScope
│   ├── src/policy.rs             · mark/crank decision logic
│   ├── src/liquidation.rs        · strict-total-order recovery queue (spec §21)
│   ├── src/coordination.rs       · FNV-1a leader election, no inter-keeper comms
│   ├── src/keeper.rs             · the tick loop (scope → coord → budget → exec → record)
│   ├── src/instruction.rs        · byte-locked v16 IX builders (tags 5/43/44/45/63)
│   ├── src/onchain.rs            · KeeperAction → Instruction bridge
│   ├── src/sender.rs             · Sender trait + RpcSender + RecordingSender
│   ├── src/realclient.rs         · live mainnet account decoder
│   └── src/bin/keeper.rs         · operator-runnable binary
│
├── covenant-percolator-bond/     ← off-chain bond library
│   ├── src/scope.rs              · canonical BondScope + sha256 hash
│   ├── src/evidence.rs           · SlashEvidence + pure verify_slash
│   ├── src/instruction.rs        · off-chain ix builders
│   ├── src/state.rs              · BondAccount POD layout (160 bytes)
│   ├── src/bridge.rs             · KeeperScope ↔ BondScope (under --features bridge)
│   └── src/program.rs            · host-testable handler simulation
│
└── covenant-percolator-bond-program/   ← SBPF on-chain program
    ├── src/lib.rs                · process_instruction dispatch
    └── tests/lifecycle.rs        · banks_client end-to-end
    └── examples/devnet_smoke.rs  · runs against live devnet program

paper/permissionless-accountable-keepers.md   ← spec-style writeup
PLAN.md                                       ← phased roadmap (10-16)
```

## Quickstart

### Run the keeper against an example market (mock client)

```bash
cd agent-os
cargo run --bin covenant-percolator-keeper -- \
  --config crates/covenant-percolator/examples/keeper.toml --once
```

Sample tick output:

```text
tick=1 decided=4 executed=4 deferred=0 skipped_capability=0
       stopped_budget=false errors=0 budget_remaining=999600
       total_receipts=4
```

### Read the live Bounty 6 mainnet market

```bash
cargo test -p covenant-percolator --features solana-rpc \
  -- live_bounty6_market_read --include-ignored --nocapture
```

Sample output (real mainnet read):

```text
Bounty 6: current_slot=422536340, last_crank_slot=422307636, assets=4
```

### Run the live devnet bond smoke test

```bash
solana airdrop 2 --url devnet         # if needed
cargo run -p covenant-percolator-bond-program --example devnet_smoke
```

Walks init → deposit → slash → verify against the deployed program. Final assertion: `bond.slashed = 1, recipient holds the slashed lamports`.

### Build the SBPF .so

```bash
cargo build-sbf --manifest-path agent-os/crates/covenant-percolator-bond-program/Cargo.toml
# produces target/deploy/covenant_percolator_bond_program.so (162 KB)
```

## Test surface

| Crate (features) | Unit | Integration |
| --- | --- | --- |
| `covenant-percolator` (default) | 18 | 8 prop |
| `covenant-percolator` (solana-rpc) | 55 | 3 pipeline + 3 network-sim + 12 prop |
| `covenant-percolator-bond` (default) | 31 | 6 prop |
| `covenant-percolator-bond` (solana + bridge) | 43 | 4 e2e + 6 prop |
| `covenant-percolator-bond-program` | 2 banks_client lifecycle | 1 live devnet smoke |

Property tests run 128 cases each — total random scenarios swept on every test run is **~3,000**.

Clippy `-D warnings` clean across all feature combinations.

## What this engages from Toly's design

Three concrete points of engagement with `aeyakovenko/percolator-prog`:

1. **Direct dependency on the v16 risk engine** (`percolator` crate, pinned at commit `323c9f27` "Harden v16 engine invariants"). We use his `HealthCertV16`, `AssetLifecycleV16`, `MarketGroupV16HeaderAccount`, `EngineAssetSlotV16Account` directly — not a paraphrase.

2. **Byte-locked instruction layout** against `v16_program.rs`. The five keeper-facing tags (5 PermissionlessCrank, 43 ForfeitRecoveryLeg, 44 RebalanceReduce, 45 FinalizeResetSide, 63 PushAuthMark) emit exactly the bytes his `Instruction::decode` reads. Golden tests pin every byte.

3. **Spec §16 + §21 implementation**. The recovery policy is fail-closed on `cert.valid = false` (§16 stale backing fails closed). The liquidation queue is a strict total order over `(certified_liq_deficit DESC, address ASC)` — explicit §21 ("no hold-and-wait, no equal-priority livelock"). Properties L1–L6 lock the behavior.

## What's deferred

- `RealPercolator::list_portfolios` — needs `getProgramAccounts` filtering against `PortfolioAccountV16Account`'s discriminator; operator-specific (indexer / rate-limit choices).
- Mainnet deploy of the bond program — devnet is the current gate. Promoting to mainnet is a one-command `solana program deploy ... --url mainnet-beta` once the design is reviewed (see [PLAN.md](../../../PLAN.md) Phase 16).
- LiteSVM end-to-end against `percolator-prog`'s own .so — would prove our keeper drives his program in-process. The risk-engine dep gives us the read path; the missing piece is loading his program binary into a banks_client.
- Whitepaper PDF + DOI — the markdown draft at `paper/permissionless-accountable-keepers.md` is the source.

## License

Apache-2.0 (same as the parent Covenant repo).
