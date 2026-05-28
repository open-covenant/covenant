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

Reproducible build via `solana-verify` against the Solana Foundation container
(Cargo 1.86, which covers the `edition2024` deps in our workspace):

```
cd <repo-root>
solana-verify build \
  --base-image solanafoundation/solana-verifiable-build:3.1.14 \
  --library-name covenant_settlement_program
solana-verify get-executable-hash agent-os/target/deploy/covenant_settlement_program.so
```

The mainnet program at `cov9UDypG7nsryxdgMcKhKU2spRVWLVjxT2iTv6do5Y` matches the
verifiable hash `def75d6991eb03f36c661cf4cdf0195d67990210ef012080db50b555b387d158`
(512,296 bytes). The verification PDA `8nHg5W1D886qpt3ffdFFsaeVq28udVauczyah9f8ckyP`
records the source repo and commit:

```
solana-verify get-program-pda --program-id cov9UDypG7nsryxdgMcKhKU2spRVWLVjxT2iTv6do5Y \
  --url https://api.mainnet-beta.solana.com
```

To reproduce + verify from scratch, anyone can run:

```
solana-verify verify-from-repo https://github.com/open-covenant/covenant \
  --program-id cov9UDypG7nsryxdgMcKhKU2spRVWLVjxT2iTv6do5Y \
  --library-name covenant_settlement_program \
  --base-image solanafoundation/solana-verifiable-build:3.1.14 \
  --commit-hash af96935cb809d013839004f6a15e5c52d31de795 \
  --mount-path agent-os \
  --url https://api.mainnet-beta.solana.com
```

Apple Silicon hosts should start colima with `--vm-type=vz --vz-rosetta` so the
amd64 container doesn't crawl under qemu emulation. Colima only auto-mounts
`$HOME`, so the source repo must be under `$HOME` (or extend the mount list);
passing `--current-dir` to `solana-verify` uses the local checkout instead of
cloning to `/tmp`.

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
