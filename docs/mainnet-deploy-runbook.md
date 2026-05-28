# Settlement program — mainnet deploy runbook

Reproducible build + deploy procedure for the Covenant settlement program.
Program ID is the same on devnet and mainnet: `cov9UDypG7nsryxdgMcKhKU2spRVWLVjxT2iTv6do5Y`
(keypair archived offline; reuse it so the ID matches across clusters).

Do not start until the launch decisions are settled: multisig authority, the
`min_stake_lock` value, fixed-rate `credits_per_covnt`, and confirmation that the
$CVNT mint is a legacy SPL Token (not Token-2022). Task escrow ships disabled
(built without the `task-escrow` feature).

## 1. Prerequisites

- Docker running (verifiable builds run in a pinned container).
- `solana-verify` (`cargo install solana-verify`) and `anchor` 0.31.1, `solana` CLI.
- The program keypair at `~/.config/solana/covenant-settlement-program.json`.
- A Squads multisig that will hold protocol authority + upgrade authority.
- The $CVNT mainnet mint address and a treasury token account for it.

## 2. Verifiable build

**Containerized verifiable build is currently blocked** — both `anchor build --verifiable`
(Cargo 1.79) and `solana-verify build` (Cargo 1.84.1) fail because our workspace
transitively requires `edition2024` (Cargo 1.85+) via `sha2 0.11` (in `covenant-audit`)
and `blake3 1.8.5` (via `litesvm` dev-dep). Both are upstream caret-version pins;
patches can't downgrade. Re-attempt once `solana-verifiable-build` updates its
container's Cargo. Until then, the launch build is reproduced by pinning the local
toolchain:

```
rustc 1.94.1 (e408947bf 2026-03-25)
solana-cli 3.1.13 (Agave)
anchor-cli 0.31.1
cd agent-os
anchor build                                                 # default, no --features
solana-verify get-executable-hash target/deploy/covenant_settlement_program.so
```

Recorded launch (escrow-off) hashes from the tooling above:
- raw sha256: `cc742d22cd572cd7ac0fd12b145829eeb93d0f572d644b88d525f506e6ecfed8`
- normalized (on-chain comparable): `265f2561d93c133bd17925a239f2a7e552b3576dafaf4ee50f1bc02ce0a4232e`
- size: 512,304 bytes

Recorded `--features task-escrow` hashes (for when escrow is re-enabled later):
- raw sha256: `767920ecce8ef46a79615e32fc9e86110ebe2b39df0457b68304f0b8ae5bc4ab`
- normalized: `dbbce952c471bfb2eb605e3c070015bd1cafefb370108548ca8d5bbc82ee4c5b`
- size: 526,616 bytes

Submit to the verified-programs registry as a follow-up once `solana-verify build`
works against this workspace.

## 3. Deploy

```
solana program deploy target/deploy/covenant_settlement_program.so \
  --program-id ~/.config/solana/covenant-settlement-program.json \
  --upgrade-authority <MULTISIG_OR_DEPLOYER> \
  --url mainnet-beta
```

If deploying with a hot deployer key, transfer upgrade authority to the multisig
immediately afterward:
`solana program set-upgrade-authority cov9UDyp... --new-upgrade-authority <MULTISIG>`.

## 4. Initialize (do this in the same operation window as the deploy)

`initialize` is permissionless until it runs once — whoever calls it first becomes
`config.authority`. Run it immediately after deploy so no one front-runs it. Set the
authority to the multisig from genesis.

Args: `slash_authority` = multisig, `credits_per_covnt` = chosen fixed rate,
`min_stake_lock` = chosen floor in seconds (e.g. 604800 for 7 days). Accounts:
config PDA, authority (signer), $CVNT mint, treasury, system program. There is no
CLI `initialize` that signs as a multisig — build the instruction in the Squads
transaction (discriminator + borsh `InitializeArgs`), or initialize with the
deployer key in the same window and then `update_authority` to the multisig.

## 5. Verify the deployed bytecode matches source

```
solana-verify verify-from-repo --url mainnet-beta \
  --program-id cov9UDypG7nsryxdgMcKhKU2spRVWLVjxT2iTv6do5Y \
  https://github.com/open-covenant/covenant
```

Confirm the reported on-chain hash equals the §2 build hash, then submit to the
verified-programs registry. (For reference, the current devnet deploy's raw dumped
`.so` hashes differently from a local non-verifiable build — expected; only the
verifiable hash is meaningful for this check.)

## 6. Post-deploy

- Set `COVENANT_PROTOCOL_PROGRAM_ID` and `NEXT_PUBLIC_COVNT_MINT` in the Render
  dashboards for the indexer and mcp-bridge (these are `sync:false`, not in repo).
- Flip `networks.mjs` / `.env` cluster defaults to mainnet when the UI cuts over.
- Smoke test with tiny amounts: `register-agent` → `open-credit-account` →
  `buy-credits`. Re-price later with `set-credits-per-covnt` if needed.

## 7. Emergency operations

- `set_pause(true)` (multisig) halts every state-mutating instruction.
- Code rollback / fix = build a new verifiable artifact and upgrade via the multisig.
- Rehearse a `set_pause` + a no-op upgrade through the multisig before launch.

## 8. Re-enabling task escrow (later, not at launch)

Escrow needs a trust-model redesign (provider recourse or a neutral arbiter) before
it is safe. Once redesigned, build with `--features task-escrow`, re-verify, and
upgrade. Until then the instructions revert with `TasksDisabled`.
