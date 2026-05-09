# covenant-indexer

Rust service that normalizes Covenant Solana program events into an HTTP snapshot.

## Run locally

```bash
cargo run
```

Health: `curl localhost:8080/healthz`

Summary: `curl localhost:8080/stats/summary`

Events: `curl localhost:8080/events`

## Config

| Env | Default | Notes |
|---|---|---|
| `COVENANT_SOLANA_RPC_URL` | `https://api.devnet.solana.com` | Solana RPC endpoint |
| `COVENANT_SOLANA_CLUSTER` | `devnet` | `localnet`, `devnet`, or `mainnet` |
| `COVENANT_PROTOCOL_PROGRAM_ID` | `CovntSettLement1111111111111111111111111111` | Covenant protocol program id |
| `COVENANT_SOLANA_CONFIRMATIONS` | `32` | Confirmation depth for indexing policy |
| `INDEXER_BIND_ADDR` | `0.0.0.0:8080` | HTTP bind address |
