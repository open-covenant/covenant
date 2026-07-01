# covenant-evm-signer

Sign Covenant statements as [EAS](https://attest.org) attestations that Base's trust stack
(Coinbase Verifications, the EAS explorer) already consumes. Covenant's canonical identity and
audit chain live on Solana; this crate re-expresses a statement in the shape an EVM verifier
reads, so one `ecrecover` authenticates it with no bridge.

## Off-chain (zero gas, no RPC)

An off-chain EAS attestation is an EIP-712 signature, not a transaction. `EasAttestationSigner`
emits two kinds, both signed by the secp256k1 issuer key:

- **Audit-root** — `attest(vc)` takes a dual-signed audit-root VC and re-signs the same statement.
  It refuses unless the key it holds is the one the VC's EVM proof recovers to, so a single
  `ecrecover` authenticates both artifacts.
- **Reputation** — `attest_reputation(projection)` signs an audit-derived score. The identity
  binding is the non-transferable Solana PDA in the payload, never an EVM token, so a score
  cannot be laundered onto a sellable NFT.

The `covenant-evm-signer` sidecar reads a statement on stdin and writes the attestation to
stdout. The key lives in this process, not the daemon's address space.

## On-chain relay (unsigned, gated)

Some verifiers must enforce *in-contract* — read the score out of EAS storage inside a
transaction. `stage_reputation_attestation(projection, chain, policy)` builds the EAS `attest`
transaction that writes the identical reputation on-chain. It is the same score, expiry, and
Solana anchor the off-chain attestation carries, so both surfaces agree.

Two boundaries are closed by construction:

- **No key, ever.** The result is an unsigned `RelayTransaction` — calldata, `to`, and `chainId`,
  with no signature, nonce, gas, or `from`. This crate never holds a signing key, signs, or
  submits. Signing and broadcast are an operator step under the operator's own key custody.
- **Sepolia autonomous, mainnet gated.** Only `BASE_SEPOLIA` stages autonomously. `BASE_MAINNET`
  returns `MainnetGated`; a mainnet write is an operator decision.

`stage-attest` is the key-free CLI: a reputation projection on stdin, the staged transaction JSON
on stdout.

```bash
echo '{"score":9500,"scoreDecimals":4,"sourceChain":"solana:5eykt4UsFv8P8NJdTREpY1vzqKqZKvdp",
       "solanaAttestationPda":"0x…","issuedAt":1700000000,"expiry":1800000000}' \
  | cargo run -p covenant-evm-signer --bin stage-attest
```

The reputation schema must be registered in the EAS SchemaRegistry on the target chain before
`attest` succeeds — itself a gated on-chain write. Until then a dry-run reverts `InvalidSchema`.

## Pins

EAS is an OP-Stack predeploy at `0x4200000000000000000000000000000000000021`, shared across the
Superchain (Base 8453 and Base Sepolia 84532 both host it), reporting `version()` `1.2.0`. The
`attest` request ABI is pinned from
[`ethereum-attestation-service/eas-contracts@v1.3.0`](https://github.com/ethereum-attestation-service/eas-contracts/tree/0c51c77cccd68e19ddbfeb832f153e75fac1af19)
(`EAS_ABI_SOURCE_COMMIT`); the `attest` selector `0xf17325e7` is derived from the signature, not
hard-coded.

## Tests

```bash
cargo test -p covenant-evm-signer --locked                       # offline unit + golden
cargo test -p covenant-evm-signer -- --ignored live_             # opt-in Base Sepolia dry-run
```

The dry-run `eth_call`s the real `attest` calldata against the live Base Sepolia EAS with no key
or funds. Staged transactions and dry-run evidence live under
[`agent-os/autonomy/multichain/staging`](../../autonomy/multichain/staging).
