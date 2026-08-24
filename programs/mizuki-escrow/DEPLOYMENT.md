# Deployment and executable verification

## Release gates

Do not deploy to mainnet until all of the following are true:

- `./scripts/test.sh` passes from a clean checkout.
- The tested artifact reports registered `EM_SBF` machine value `263`, SBPFv2 ELF flags (`0x2`), and no unresolved-symbol diagnostics.
- The policy signer passes its conformance test against `abi/mizuki-escrow-v1.json`.
- A containerized `solana-verify build` produces the approved executable hash.
- Devnet canaries cover fund-before-advertise, bind, exact release, unbound refund, bound refund, wrong claimant, expired release, replay, and state/vault rent recovery.
- Two independent finalized RPC providers return the same program bytes and program metadata.
- The production authority and fee payer are held outside the API and updater services.

Status on 23 August 2026: the hosted build/test gate and the listed devnet behavior canaries passed for artifact SHA-256 `2d24fd43b65a7bb31b39007b93717b1f65615df39aeec33b9eebe83bb89a2237`. Independent review, an independent reproducible build, an approved immutable mainnet deployment, and two-RPC mainnet verification have not passed. Mainnet remains blocked.

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

### Immutable hosted build evidence

Every push to protected `main` builds the escrow program in the pinned container and publishes `mizuki-escrow-COMMIT` as an immutable GitHub Release. The release contains the exact SBPF artifact, ABI, Cargo manifests, build image and toolchain records, ELF/SBPF metadata, Solana executable hash, source and workflow commits, complete SHA-256 manifests, and a GitHub provenance bundle. Repository immutable releases must remain enabled. Publication fails if the commit-addressed tag already exists without its release or if GitHub does not report the published release as immutable.

An operator-authenticated check on 23 August 2026 returned `{"enabled":true,"enforced_by_owner":false}`. This is point-in-time ceremony evidence, not a durable guarantee. Before every release-producing merge, an administrator must run `gh api -H 'X-GitHub-Api-Version: 2026-03-10' repos/open-covenant/covenant/immutable-releases | jq -e '.enabled == true'` and retain the result with the deployment evidence. GitHub's workflow token cannot read this administration endpoint, so CI enforces the observable result instead: the published release must report `immutable: true`. If it does not, the run fails, the release must never authorize deployment, and release production stays blocked until immutability is restored and a new source commit produces a new tag.

The release job is the only job with write and attestation permissions, and it runs only for a push to `main` in the canonical repository. Pull-request jobs remain read-only. A rerun that finds an immutable release never replaces its tag or assets: it downloads the release, compares every rebuilt payload byte, validates every GitHub asset digest, resolves the tag to the exact source commit, and verifies the original provenance bundle against `.github/workflows/mizuki.yml`. An interrupted mutable draft is validated, rebuilt, and published only after its complete asset set passes digest checks.

To verify a release independently, fetch it through the GitHub API and require the exact tag, `draft: false`, `prerelease: false`, and `immutable: true`. Resolve `git/ref/tags/TAG`, dereference an annotated tag if present, and require its commit to equal the expected 40-character source commit; `target_commitish` is not authoritative. After downloading the exact asset set, run `sha256sum --check --strict RELEASE_SHA256SUMS`. Then run `gh attestation verify` for every asset other than `GITHUB_PROVENANCE.json` and `RELEASE_SHA256SUMS`, passing `--bundle GITHUB_PROVENANCE.json`, `--repo open-covenant/covenant`, `--signer-workflow open-covenant/covenant/.github/workflows/mizuki.yml`, the expected workflow and source commit digests, `--source-ref refs/heads/main`, and `--deny-self-hosted-runners`.

This release proves what the hosted build produced. It is not evidence that the program is deployed on mainnet or immutable on-chain; the deployment and two-RPC verification receipts below remain separate release gates.

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

### Completed canary evidence

The 23 August 2026 live canary used revision `dfeffb0a8c8280bb7b3844bead750fccf7233ae7` and [devnet program `3yA83Hkj1e78J54n6DBGEJonB9Fug3XRwjGwEzxShfHn`](https://explorer.solana.com/address/3yA83Hkj1e78J54n6DBGEJonB9Fug3XRwjGwEzxShfHn?cluster=devnet). The runner matched all 104,376 deployed bytes to the hosted artifact before execution. The final receipt reports payload digest `34d2dfd348e480477a06c2b3f082b33667aeef1f88909b92e3d6b1b2451a5e67`; the pre-execution recovery journal reports payload digest `849cb9504be370dbc3ec5439692988250cbe920058d3a45dbd7fd95b3d117cc1`.

Nine intended transactions finalized without error. Four negative transactions finalized with the expected program error, covering wrong claimant, expired release, release replay, and fund replay. Each terminal flow verified exact principal movement, closed state and vault accounts, and a persistent terminal guard. Public transaction links are recorded in the [production-readiness audit](../../docs/production-audit-mizuki.md#live-devnet-escrow-canary).

The devnet program still has an upgrade authority. This evidence must be regenerated if the approved artifact, program ID, or canary runner changes, and it must not be presented as an immutable deployment, third-party review, or commercial paid/refund canary.

## Current mainnet status

Mainnet deployment is a hard **NO-GO**. No approved mainnet program ID, independent review, independent build receipt, funded deployment ceremony, or two-RPC finalized program-data receipt exists. The rent estimate observed on 23 August 2026 for the 104,421-byte program-data account was 0.72766104 SOL; re-query rent immediately before the ceremony and fund only from an explicitly approved project source.

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

1. Both endpoints must use HTTPS and different registrable provider domains. IP literals, single-label hosts, redirects, different paths, query tokens, private-suffix tenants, or subdomains under one provider domain do not count as independent providers.
2. A direct JSON-RPC preflight that refuses redirects requires both endpoints to return the canonical mainnet-beta genesis hash `5eykt4UsFv8P8NJdTREpY1vzqKqZKvdpKuc147dw2N9d`.
3. `solana program show --commitment finalized --output json` is captured from both RPCs. The verifier requires the expected loader, a null upgrade authority, and identical program-data metadata or exits nonzero.
4. `solana program dump --commitment finalized` extracts executable bytes from each program-data account. The two dumps must be byte-identical and match the approved local artifact.
5. SHA-256 is recorded for the local file and both finalized dumps.
6. `solana-verify get-program-hash` is calculated independently for the local and both on-chain executables. The verifier requires all three values to match or exits nonzero. This hash is distinct from raw file SHA-256; compare like with like.

The retained directory includes the two provider domains and both genesis observations, but never stores full RPC URLs or their credentials.

The signer independently checks the same full mainnet-beta genesis hash through both provider domains during startup readiness and before every financial read or mutation. It also repeats loader-state parsing: resolve the executable program to its program-data address, require the expected loader owner, require no upgrade authority, hash the executable bytes, compare them to its allowlist, and require the same result from two finalized RPCs. Any mismatch keeps settlement mutations disabled.

## Post-deployment canaries

Use new low-value bounty IDs for every canary. The first mainnet sequence should be public and limited to:

1. Fund an unbound bounty and verify the public API remains closed until finalized chain state is observed.
2. Bind a controlled contributor wallet, merge qualifying work, and release exactly the recorded principal.
3. Fund a second bounty, leave it unbound until expiry, and execute a full refund.
4. Confirm state and vault accounts are closed, the terminal guards remain, all transaction signatures are finalized, and ledger entries include the irrecoverable guard rent.
