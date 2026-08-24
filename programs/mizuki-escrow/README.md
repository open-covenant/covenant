# Mizuki contributor escrow

This native Solana program makes an advertised Mizuki bounty actually funded. Mizuki is a male maintenance agent; he may publish a bounty only after the authority has placed its exact SOL principal in the canonical on-chain vault.

The program has four instructions and no claimant-callable path:

1. `fund` creates an unbound escrow, vault, and permanent replay guard.
2. `bind` lets the same authority bind one claimant and one wallet-proof commitment exactly once.
3. `release` lets the same authority pay the immutable claimant before claim expiry.
4. `refund` lets the same authority recover the principal once the applicable expiry has arrived.

At claim expiry the paths do not overlap: release requires `now < claim_expires_at`; refund requires `now >= claim_expires_at`. A successful terminal instruction closes the full state and vault, returns their rent and any unsolicited vault surplus to the authority, and leaves a 108-byte terminal guard. The guard permanently blocks recreation, replay, or a second claimant for that authority and bounty digest.

The program deliberately has no configuration account, authority rotation, destination change, partial payout, claimant timeout, dispute instruction, close instruction, arbitrary CPI, token support, or upgrade shortcut. Fund is the only path that performs CPI, and it invokes only the fixed System Program to create canonical PDAs.

Funding safely adopts canonical PDAs that were prefunded with lamports while still owned by the System Program and holding no data. It tops them up when necessary, allocates them, and assigns them to the escrow program in the same atomic transaction. Occupied or non-System accounts remain invalid. This prevents third-party dust transfers from blocking a known bounty ID.

## Source of truth

[`abi/mizuki-escrow-v1.json`](abi/mizuki-escrow-v1.json) is the machine-readable wire contract. It contains exact instruction lengths, discriminators, account ordering, PDA seeds, state layouts, time boundaries, and golden byte vectors. The Rust codec and the external signer must both pass conformance tests against this file.

External bounty IDs are encoded as:

```text
sha256(utf8(`mizuki:bounty:v1:${bountyId}`))
```

Commitments and evidence are opaque non-zero 32-byte hashes to the program. Their preimages belong in the signed policy-signer receipt; the program prevents mutation and commits each full 236-byte state version into the replay guard with:

```text
sha256(utf8("mizuki:escrow:state:v1") || state_bytes)
```

## Build and test

```bash
./scripts/test.sh
```

The program tests load the built SBF artifact into LiteSVM and exercise real System Program CPIs, state transitions, lamport movement, strict expiry boundaries, terminal closure, pre-fund PDA dusting, donation griefing, alternate destinations, wrong authority/accounts/bumps, rebind attempts, malformed data, underfunded vaults, and replay.

The test gate requires `cargo-build-sbf` 4.0.0 or newer with platform-tools 1.53 or newer. It builds with `--arch v2`, rejects unresolved-symbol diagnostics, requires the registered `EM_SBF` machine value `263` and ELF flags `0x2`, and then runs six host tests plus 25 SBF-backed tests against that exact artifact. Set `CARGO_BUILD_SBF_BIN` only when the compatible builder is installed outside `PATH`.

For a containerized reproducible build:

```bash
./scripts/build-verifiable.sh
```

Deployment and independent program-data verification are in [`DEPLOYMENT.md`](DEPLOYMENT.md).

## Rent economics

At the current network rent schedule used by the Solana CLI:

- 236-byte active state: 2,533,440 lamports, reclaimed at resolution.
- 40-byte active vault: 1,169,280 lamports, reclaimed at resolution.
- 108-byte permanent guard: 1,642,560 lamports, intentionally irrecoverable.

Those values must be queried again at quote time. The bounty quote must include the temporary rent float and treat guard rent as an on-chain cost, not as bounty principal.

## Toolchain

The program pins `solana-program` 3.0.0. The runtime suite pins LiteSVM 0.15.2 and its compatible Agave runtime family at 4.1.2 so the production SBPFv2 executable is tested. SBPFv2 is selected because it is active on the target cluster; the release gate must be revisited before changing architectures. The verifiable build pins the Solana 4.0.0 container by immutable image digest.

## Devnet evidence

On 23 August 2026, the exact 104,376-byte hosted artifact from revision `dfeffb0a8c8280bb7b3844bead750fccf7233ae7` was deployed to devnet and passed the live artifact canary. Its SHA-256 is `2d24fd43b65a7bb31b39007b93717b1f65615df39aeec33b9eebe83bb89a2237`. The canary finalized prefunded release, bound-expiry refund, and unbound-expiry refund flows, including wrong-claimant, expired-release, and replay rejections. It also verified exact principal movement, state and vault closure, and the permanent terminal guards.

The devnet program is upgradeable and is not a production deployment. Transaction links, receipt digests, and the exact evidence boundary are recorded in the [production-readiness audit](../../docs/production-audit-mizuki.md#live-devnet-escrow-canary). Independent review, an independent reproducible-build/hash match, immutable mainnet deployment, and two-RPC program-data verification remain mandatory before the signer can enable production escrow.
