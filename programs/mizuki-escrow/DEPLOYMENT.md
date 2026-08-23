# Deployment and executable verification

## Release gates

Do not deploy to mainnet until all of the following are true:

- `./scripts/test.sh` passes from a clean checkout.
- The tested artifact reports SBPFv2 ELF flags (`0x2`) and the build log contains no unresolved-symbol diagnostics.
- The policy signer passes its conformance test against `abi/mizuki-escrow-v1.json`.
- A containerized `solana-verify build` produces the approved executable hash.
- Devnet canaries cover fund-before-advertise, bind, exact release, unbound refund, bound refund, wrong claimant, expired release, replay, and state/vault rent recovery.
- Two independent finalized RPC providers return the same program bytes and program metadata.
- The production authority and fee payer are held outside the API and updater services.

## Program ID strategy

Generate separate devnet and mainnet program keypairs outside the repository. Never commit or copy either into application containers. The mainnet keypair's public key becomes the production program ID; the program has no embedded ID or configurable admin account. The policy signer starts with escrow disabled and receives the mainnet program ID only after immutable deployment and hash verification.

```bash
solana-keygen new --outfile /secure/devnet/mizuki-escrow-program-keypair.json
solana-keygen new --outfile /secure/offline/mizuki-escrow-mainnet-program-keypair.json
solana-keygen pubkey /secure/devnet/mizuki-escrow-program-keypair.json
solana-keygen pubkey /secure/offline/mizuki-escrow-mainnet-program-keypair.json
```

The paths above are illustrative. Use the project's actual offline signing procedure and record that the two public keys differ. Never use the mainnet program-ID signer, deploy authority, upgrade authority, or fee payer on devnet. The mainnet program-ID signer is needed for initial deployment only; keep it offline before and after that ceremony.

## Build

```bash
./scripts/test.sh
./scripts/build-verifiable.sh
./scripts/hash-artifact.sh
```

Record the Git commit, `Cargo.lock`, Solana CLI version, `solana-verify` version, SHA-256, and Solana executable hash in the release receipt.

The local production gate requires `cargo-build-sbf >= 4.0.0` and `platform-tools >= 1.53` and always supplies `--arch v2`. The verifiable build is pinned to `solanafoundation/solana-verifiable-build:4.0.0` at digest `sha256:0b4e3716fad9ca4b4aac3e3f977f43aad93a18c22296c0c0f44fc22e644bdd68`. Review and update that digest explicitly; never accept a mutable image tag as release evidence.

## Devnet

Deploy upgradeable on devnet only while canaries are running. Confirm the program ID explicitly and never rely on a CLI-generated random address.

```bash
solana program deploy \
  --url devnet \
  --program-id /secure/devnet/mizuki-escrow-program-keypair.json \
  target/deploy/mizuki_escrow_program.so
```

After all canaries, either finalize the devnet program or abandon it. Never reuse any devnet program-ID, deploy, fee-payer, or upgrade-authority key material in production.

Download the CI artifact and its checksum instead of rebuilding the canary input locally. Run the policy signer's devnet artifact canary in dry-run mode, review the redacted receipt, then rerun with `--execute` and a fresh receipt path. The runner independently checks the devnet genesis hash and requires finalized upgradeable-loader-v3 program data to equal that exact downloaded SBPFv2 artifact before it can submit a transaction. It does not deploy, create keys, request funds, or accept a mainnet RPC. Usage and evidence fields are documented in `services/mizuki-policy-signer/README.md`.

## Mainnet immutable deployment

The production deployment must be immutable from its first successful deploy:

```bash
solana program deploy \
  --url mainnet-beta \
  --final \
  --program-id /secure/offline/mizuki-escrow-mainnet-program-keypair.json \
  target/deploy/mizuki_escrow_program.so
```

Do not pass `--skip-feature-verify`. If an operational process requires a staged upgradeable deploy, keep escrow creation disabled and execute `solana program set-upgrade-authority PROGRAM_ID --final` before the signer allowlist is enabled.

## Finalized program-data verification

Run the repository verifier with two unrelated finalized RPC endpoints:

```bash
./scripts/verify-program-data.sh PROGRAM_ID RPC_A RPC_B
```

The procedure performs these independent checks:

1. `solana program show --commitment finalized --output json` is captured from both RPCs. The verifier requires the expected loader, a null upgrade authority, and identical program-data metadata or exits nonzero.
2. `solana program dump --commitment finalized` extracts executable bytes from each program-data account. The two dumps must be byte-identical and match the approved local artifact.
3. SHA-256 is recorded for the local file and both finalized dumps.
4. `solana-verify get-program-hash` is calculated independently for the local and both on-chain executables. The verifier requires all three values to match or exits nonzero. This hash is distinct from raw file SHA-256; compare like with like.

The signer must independently repeat loader-state parsing at startup: resolve the executable program to its program-data address, require the expected loader owner, require no upgrade authority, hash the executable bytes, compare them to its allowlist, and require the same result from two finalized RPCs. Any mismatch keeps escrow mutations disabled.

## Post-deployment canaries

Use new low-value bounty IDs for every canary. The first mainnet sequence should be public and limited to:

1. Fund an unbound bounty and verify the public API remains closed until finalized chain state is observed.
2. Bind a controlled contributor wallet, merge qualifying work, and release exactly the recorded principal.
3. Fund a second bounty, leave it unbound until expiry, and execute a full refund.
4. Confirm state and vault accounts are closed, the terminal guards remain, all transaction signatures are finalized, and ledger entries include the irrecoverable guard rent.
