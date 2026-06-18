# Covenant × zauth

[zauth](https://zauth.inc) runs a provider hub for x402: a directory that
discovers public x402 endpoints, health-checks them, and exposes provider
telemetry. Covenant integrates with it so that the paid resources Covenant
exposes are discoverable and monitored, and so Covenant agents can read the
directory when looking for paid services.

## Crate

`covenant-zauth` (in `agent-os/crates/`) is the Rust client:

- **Directory client** — query the zauth provider hub for registered x402
  endpoints (URL, network, health).
- **x402 v2 header decoder** — decode the `payment-required` challenge an x402
  resource returns, into the typed requirements Covenant signs against.
- **RepoScan request builder** — construct RepoScan requests against the zauth
  verifier for an endpoint under test.

The signing path reuses `covenant-x402` (`PayaiSolanaSigner`); zauth adds no
new key handling.

## Live integration

Covenant's public x402 seller (`https://x402-seller.opencovenant.org`, see
[Covenant x402](./x402.md)) is registered and health-monitored in the zauth
provider hub on Solana mainnet. The hub lists the endpoint, periodically
health-checks the registered URL, and tracks provider telemetry reported by the
`@zauthx402/sdk` provider middleware mounted on the seller.

One operational detail worth stating plainly: the hub registers a selling
endpoint only once it processes a **real settled payment**, not on SDK install
or on `402` challenge telemetry. An endpoint appears in the directory after its
first successful on-chain settlement, and drops out when its health check stops
passing.

## See also

- [Covenant x402](./x402.md) — the payment layer zauth discovers and monitors.
- [Hyre x402 Provider](./hyre-integration.md) — a paid provider on the outbound path.
