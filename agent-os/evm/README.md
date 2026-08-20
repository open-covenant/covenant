# covenant-erc8004-register

Build and dry-run the calldata to register a Covenant agent in the EVM-native
[ERC-8004](https://eips.ethereum.org/EIPS/eip-8004) Identity Registry. Covenant's canonical
identity lives on Solana; this is an outward-facing handle that makes the agent discoverable
in EVM tooling. **It stops at the transaction boundary** — it never holds a key, signs, or
submits. Signing and on-chain submission are operator-gated.

This crate is intentionally **detached from the `agent-os` workspace** (its own `[workspace]`
and lockfile). It is on-chain staging tooling, not one of the in-workspace off-chain trust
crates, and it keeps the workspace dependency graph untouched.

## Pins

Addresses and ABI are pinned from `erc-8004/erc-8004-contracts@68fc6765761a10fb26f0692df21c8a6f9d12b1be`
(see [`deployments.json`](deployments.json)):

| Network | Chain | IdentityRegistry |
|---|---|---|
| Base | 8453 | `0x8004A169FB4a3325136EB29fA0ceB6D2e539a432` |
| Base Sepolia | 84532 | `0x8004A818BFB912233c491871b3d84c89A494BD9e` |
| Ethereum Sepolia | 11155111 | `0x8004A818BFB912233c491871b3d84c89A494BD9e` |

`register(string agentURI) -> uint256 agentId`, selector `0xf2c298be`. The registries are
**upgradeable ERC-721 proxies**: the address is stable, the implementation behind it can change.

## Usage

```bash
# Emit a staged call as JSON (reproducible calldata; unsigned).
cargo run --bin stage-register -- --agent-uri ipfs://<cid> --chain-id 8453

# Offline unit + golden-vector tests.
cargo test

# Opt-in live dry-run: read-only eth_call against Base Sepolia, no key or funds.
cargo test -- --ignored live_
```

Staged registrations and the Base Sepolia dry-run evidence live under
[`agent-os/autonomy/multichain/staging`](../autonomy/multichain/staging).
