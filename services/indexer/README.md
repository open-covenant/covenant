# covenant-indexer

**Status: fixture mode.** This service is the HTTP shell that a future Solana
indexer will live behind. It does not currently connect to Solana RPC.
`/events` returns a seeded snapshot so downstream consumers (web console,
SDK demos, integration fixtures) can exercise the wire shape before the
real subscriber lands.

The on-chain program is in active flux (settlement program events grew
`TaskRefunded` and `StakeWithdrawn` in recent changes); wiring a real
`programSubscribe` consumer is deferred until the on-chain shape and IDL
stabilize. Consumers can detect fixture mode via the `mode: "fixture"`
field on `/healthz` and `/stats/summary`.

## Run locally

```bash
cargo run
```

| Endpoint | Returns |
|---|---|
| `GET /healthz` | `{ ok, chain, cluster, rpc_url, confirmations, latest_slot, indexed_events, mode }` |
| `GET /stats/summary` | `{ chain, cluster, latest_slot, indexed_events, mode }` |
| `GET /events` | `Vec<SolanaEventRecord>` — seeded fixtures |

## Config

| Env | Default | Notes |
|---|---|---|
| `COVENANT_SOLANA_RPC_URL` | `https://api.devnet.solana.com` | Surfaced on `/healthz`; not yet polled |
| `COVENANT_SOLANA_CLUSTER` | `devnet` | `localnet`, `devnet`, or `mainnet` |
| `COVENANT_PROTOCOL_PROGRAM_ID` | `CovntSettLement1111111111111111111111111111` | Covenant settlement program id |
| `COVENANT_SOLANA_CONFIRMATIONS` | `32` | Confirmation depth the live indexer will require once wired |
| `INDEXER_BIND_ADDR` | `0.0.0.0:8080` | HTTP bind address |

## Path to live indexing

When the settlement program reaches deployed-and-frozen on devnet:

1. Add `solana-client` + `solana-pubsub-client` to `Cargo.toml`.
2. Spawn a tokio task that subscribes to `logsSubscribe` filtered by
   `COVENANT_PROTOCOL_PROGRAM_ID`.
3. Parse program-log lines into `LogEnvelope`, run through `normalize_event`,
   append to a shared store (SQLite via `rusqlite`, or Postgres if the
   broader project standardizes).
4. Persist the last processed slot so a restart doesn't replay or skip rows.
5. Flip `mode` from `"fixture"` to `"live"` and drop the seeded payload.
