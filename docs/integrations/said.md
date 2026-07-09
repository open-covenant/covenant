# SAID integration: identity, registration, audit-anchored execution

Status: shipped (lookup, free-tier, inbox, send, anchor fixture, plus `register_agent` and `get_verified` live on mainnet). `submit_anchor` and `validate_work` are built, gated, and report `BridgeUnsupportedError` until said-sdk publishes those instructions.

Crates: `agent-os/crates/covenant-said-bridge` (Rust) + `packages/said-bridge` (TypeScript worker).

## Summary

[SAID Protocol](https://saidprotocol.com) is the agent identity layer on Solana. An agent registers a wallet on chain, pays the 0.01 SOL verification fee, lands in a public directory, and accrues an on-chain reputation score. This crate plugs that identity into Covenant so the agent's Covenant audit trail becomes the proof of behavior beneath SAID's badge.

- **Read-only (on by config).** REST against `api.saidprotocol.com` for the agent lookup, the xchain inbox poll, the xchain free-tier check, and the outbound xchain send. No paid path. Surfaces as `covenant said lookup` / `inbox` / `free-tier` / `send`.
- **Paid (per-instruction gated).** Four on-chain instructions guarded by `COVENANT_SAID_ALLOW_PAID_*` flags. `register_agent` and `get_verified` work today against the live SAID program on mainnet (`5dpw6KEQPn248pnkkaYyWfHwu2nfb3LUMbTucb6LaA8G`). `submit_anchor` and `validate_work` are wired end-to-end but report a clean `BridgeUnsupportedError` envelope because the published said-sdk does not expose them yet (see [Dependency posture](#dependency-posture)).

SAID owns identity. Covenant adds the audit-rooted accountability layer underneath. The two never share keys: SAID instructions are signed by `COVENANT_SAID_KEYPAIR`, Covenant settlement is signed by the operator key. They can rotate independently.

## Enable it

The daemon constructs the bridge when `COVENANT_SAID_ENABLED` is truthy. Everything is off by default.

| Variable | Default | Effect |
| --- | --- | --- |
| `COVENANT_SAID_ENABLED` | off | Master toggle. Every verb refuses when off. |
| `COVENANT_SOLANA_CLUSTER` | `devnet` | `mainnet` or `devnet`. Selects the SAID program ID and the default RPC. |
| `COVENANT_SAID_KEYPAIR` | unset | Path to a Solana CLI keypair file (64-byte JSON array). The wallet inside becomes the agent's SAID owner. |
| `COVENANT_SAID_MAINNET_RPC_URL` | `api.mainnet-beta.solana.com` | Falls back to `COVENANT_SAID_RPC_URL`. |
| `COVENANT_SAID_DEVNET_RPC_URL` | `api.devnet.solana.com` | Same. |
| `COVENANT_SAID_API_BASE_URL` | `https://api.saidprotocol.com` | Validated as `http(s)` at boot; an invalid value falls back to the default. |
| `COVENANT_SAID_ALLOW_PAID_REGISTER` | off | Open the `register_agent` gate. |
| `COVENANT_SAID_ALLOW_PAID_VERIFY` | off | Open the `get_verified` gate (0.01 SOL fee per call). |
| `COVENANT_SAID_ALLOW_PAID_ANCHOR` | off | Reserved for `submit_anchor` when said-sdk exposes it. |
| `COVENANT_SAID_ALLOW_PAID_VALIDATE_WORK` | off | Reserved for `validate_work` when said-sdk exposes it. `COVENANT_SAID_ALLOW_PAID_VALIDATE` is accepted as a legacy alias. |
| `COVENANT_SAID_WORKER_CMD` | `covenant-said-worker` | Override to point at a local build, e.g. `node /abs/path/dist/worker.js`. |
| `COVENANT_SAID_WORKER_TIMEOUT_SECS` | 30 | Subprocess wall-clock cap. |
| `COVENANT_SAID_REST_TIMEOUT_SECS` | 15 | REST request cap. |

```
COVENANT_SAID_ENABLED=1 \
COVENANT_SOLANA_CLUSTER=mainnet \
COVENANT_SAID_KEYPAIR=$HOME/.config/solana/covenant-agent.json \
covenantd
```

The TS worker reads the same variables. See `packages/said-bridge/README.md` for the worker's stdin contract.

## Reads: lookup, free-tier, inbox, send

These run in-process against `api.saidprotocol.com` over `reqwest` with a default 15-second timeout and a 1 MiB response body cap (pre-checked against `Content-Length`, re-checked post-read). Wallet and address path segments are validated as base58 before they reach the URL builder, so an IPC peer cannot steer the request off-path.

```
covenant said lookup --wallet AdChc…
covenant said free-tier --address AdChc…
covenant said inbox --chain solana --address AdChc…
covenant said send --source-chain solana --source-address AdChc… \
                   --target-chain base --target-address 0x… \
                   --payload '{"hello":"world"}'
```

## Paid: register, verify, anchor, validate-work

The four on-chain instructions route through the `@covenant/said-bridge` TS worker as a subprocess. Each is double-gated: `COVENANT_SAID_ENABLED` must be on, and the per-instruction `COVENANT_SAID_ALLOW_PAID_*` flag must be on. Worker stdout is parsed for the first JSON envelope and capped at 1 MiB; stderr capped at 256 KiB; the subprocess is killed on drop when the timeout fires.

**Live on mainnet today:**

```
covenant said register --metadata-uri https://opencovenant.org/.well-known/said-agent.json
covenant said verify
```

Verified live on `AdChcSmDKX57rU9qChMJ3MKnqNZbmiQAjuns9VCjzqRb`, agent PDA `WexMEMKv1dRttTGcZUdn6ugxzvdUnjrtj5c1iEZLhDC`. Register tx `3tD6Ab8…`, verify tx `479fec3…`. SAID's REST indexer now resolves the agent at `/api/agents/AdChc…`.

**Pending said-sdk (built, gated, errors cleanly):**

```
covenant said anchor --start 0 --end 4 --root <64-hex>
covenant said validate-work --agent <pubkey> --task-hash <64-hex> --passed --evidence ipfs://…
```

Both return `BridgeUnsupportedError`. The bridge wiring, the SQLite-backed anchor cursor, the operator gates, and the typed payloads are complete. The moment said-sdk publishes `submitAnchor` and `validateWork`, the worker picks them up on a peer-dep bump.

## Anchor cursor

`submit_anchor` requires `anchor_index = AgentIdentity.last_anchor_index + 1` on the SAID side. The bridge keeps a SQLite cursor at `$COVENANT_HOME/said/cursor.db` that persists every claim before submit and every confirmation after.

Two invariants worth knowing for review:

- `confirm` only updates a row whose `tx_sig` is still `NULL`. A second confirm for the same `anchor_index` rejects rather than silently overwriting.
- If a prior anchor claim is still pending (worker failed or daemon crashed mid-submit), the next anchor call refuses with `BridgeError::Invalid("anchor cursor has pending claim at index N; reconcile before submitting")`. The cursor cannot silently skip a poisoned index. A successful on-chain submit whose local confirm fails is logged at `tracing::error` with the `tx_sig` and `slot` before the error bubbles, so the operator has the receipt to reconcile manually.

Fixture mode (`covenant said anchor --start 0 --end 4 --root <hex>`, no `--live`) writes the same payload to `anchor_pending.jsonl` instead of broadcasting. No SOL spent. Useful for shaping the audit-slice pipeline before opening the paid gate.

## Dependency posture

SAID's on-chain program is the source of truth. The bridge depends on `said-sdk` for the on-chain instruction builders only, because the program's account layout is not yet documented for external reimplementation. Two consequences worth flagging:

- **`AGENT_ACCOUNT_SIZE` is stale.** `said-sdk@0.3.4` checks `accountInfo.data.length !== 263` inside `lookup` / `lookupByPDA` and early-returns `null`. The on-chain `Agent` account from a fresh `RegisterAgent` is 342 bytes, so the SDK reports our agent as missing while the PDA reads as registered + verified on chain. The bridge sidesteps this by using SAID's REST API (`/api/agents/:wallet`) for lookups rather than the SDK. Once the SDK constant is corrected, the bridge can drop the REST hop.
- **`submitAnchor` and `validateWork` are not in the published SDK.** The bridge ships the full surface so an operator can wire it now; the verbs report `BridgeUnsupportedError` until the SDK or the program publishes the instruction. Both findings are with the SAID team.

If the SAID program publishes its account layout (or the SDK adds the missing methods), the dependency risk drops to zero and the bridge becomes self-contained.

## Trust boundary

SAID asserts identity: this wallet registered, paid the fee, lives in the directory, accrues a reputation score from its on-chain history. That assertion is signed by the SAID program at registration time and is independently verifiable on chain.

Covenant adds the behavior layer underneath: scoped capabilities, simulated before signing, signed when run, anchored after. The Covenant audit trail lives in its own immutable log signed by the operator's audit identity.

The two trust roots stay separate. A SAID badge says **who**. A Covenant audit row says **what**. A consumer can weigh both; neither is laundered into the other.

## Scope

The crate consumes SAID as the identity layer only. SAID's broader features (multi-wallet authority transfer, the social leaderboard, cross-chain message routing beyond the explicit `send`/`inbox` verbs) are out of scope. The Covenant agent identity stays bound to the operator key; SAID's on-chain authority transfer is not used.

## Where to review first

If the diff is unfamiliar, read in this order:

1. `agent-os/crates/covenant-said-bridge/src/config.rs` — the env contract.
2. `agent-os/crates/covenant-said-bridge/src/path.rs` — wallet and chain validation at the trust boundary.
3. `agent-os/crates/covenant-said-bridge/src/cursor.rs` — the anchor cursor and its invariants.
4. `agent-os/crates/covenant-said-bridge/src/worker.rs` — the subprocess transport and envelope parsing.
5. `packages/said-bridge/src/index.ts` — the said-sdk adapter.
6. `agent-os/crates/covenantd/src/lib.rs` — the SAID dispatch handlers (search `said_`).
7. `agent-os/crates/covenantd/tests/said_dispatch.rs` — the contract every handler is held to.
