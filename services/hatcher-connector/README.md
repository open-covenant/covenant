# @covenant/hatcher-connector

Local-first execution + governance connector. Hatcher's **hosted** agents drive a
**local** Covenant daemon over an **outbound** WebSocket, so the daemon never listens
on a public interface. Hatcher dispatches an intent → the connector mints
least-privilege capabilities, runs it as `POST /intent` on `covenantd`, relays the
live SSE trace, and returns an audit-rooted proof envelope.

The 6 integration deliverables for the Hatcher team: [`docs/hatcher-handoff.md`](../../docs/hatcher-handoff.md).

## Status

- **REAL + live-validated end-to-end against a real `covenantd`:** dispatch → grant →
  intent → SSE/terminal handling → result → audit-root proof; A2A bridge; outbound
  `WsTransport`; pairing leg-3 (`POST /identity/sign`, ed25519-verified). 32 tests
  (incl. real-socket mesh integration), typecheck clean.
- **Reference Hatcher mesh:** `src/demo/meshServer.ts` — a real `ws` server speaking the
  `covenant.connector-mesh.v1` frame contract (confirmed with Hatcher), with an HTTP
  control plane (`pnpm mesh`) so you can drive dispatches by hand.
- **Confirmed (C2):** Hatcher confirmed the `covenant.connector-mesh.v1` frame contract
  (2026-06-04) and the connector implements it (connector_id, intent.context, structured
  grants, success/failed/cancelled status, enriched proof). Awaiting their staging
  endpoint for live transport testing.
- **Roadmap:** scope-predicate depth (path/argv/domain), gateway→daemon tool delegation
  for token-gated fs/terminal, sandbox hardening. See handoff §§5–6.

## Layout

| File | Role |
|---|---|
| `src/config.ts` | env-only config |
| `src/daemon.ts` | verified covenantd client (intent/SSE/result/audit/capabilities/A2A/attestation) |
| `src/manifest.ts` | `covenant.hatcher-agent.v0` schema (zod) |
| `src/capabilities.ts` | manifest → enforced grants + consent policy |
| `src/transport.ts` | `HatcherTransport` interface + in-memory `StubTransport` (tests) |
| `src/wsTransport.ts` | real outbound WebSocket transport (the shipped entrypoint) |
| `src/a2a.ts` | Hatcher mesh ↔ Covenant A2A mailbox bridge |
| `src/pairing.ts` | pairing leg-3 (sign `nonce‖code‖pubkey` via `/identity/sign`) |
| `src/connector.ts` | dispatch → grant → intent → trace relay → proof |
| `src/index.ts` | health server + connector boot (WsTransport, manifest loading) |
| `src/demo/` | scripted/offline demo, real-WS demo, reference mesh server + control plane |

## Env

| Var | Required | Default |
|---|---|---|
| `HATCHER_CONNECTOR_TOKEN` | yes (≥24 chars; must equal the mesh's expected auth) | — |
| `COVENANT_OPERATOR_TOKEN` | yes, unless `COVENANT_CONNECTOR_TOKEN` is set | — |
| `COVENANT_CONNECTOR_TOKEN` | no (scoped peer; **preferred** — operator doesn't constrain) | — |
| `COVENANT_DAEMON_URL` | no | `http://127.0.0.1:8421` |
| `HATCHER_MESH_URL` | no | `wss://mesh.hatcher.local/connector` |
| `HATCHER_MANIFEST` | no (path to a manifest JSON) | — |
| `HATCHER_PAIRING_CODE` | no | — |
| `HATCHER_AGENT_ID` | no | — |
| `PORT` | no | `8790` |

## Dev

```sh
pnpm install --ignore-workspace   # use pnpm, not npm (pnpm node_modules layout)
pnpm typecheck && pnpm test && pnpm build
```

## Run it as three processes (local end-to-end)

Needs a built `covenantd` (`cargo build -p covenantd` in `agent-os/`) and `pnpm build` here.

```sh
# 0) build the connector
pnpm build

# 1) the daemon — fresh home + live trace
HOME_DIR=$(mktemp -d)
COVENANT_HOME=$HOME_DIR COVENANT_LIVE_TRACE=1 \
  ../../agent-os/target/debug/covenantd &        # http://127.0.0.1:8421
TOKEN=$(cat "$HOME_DIR/peers/operator.token")

# 2) the reference Hatcher mesh (real ws server + control plane)
MESH_AUTH=hatcher-connector-demo-auth-token MESH_PORT=8788 MESH_CONTROL_PORT=8789 \
  pnpm mesh &                                    # ws://127.0.0.1:8788  +  http://127.0.0.1:8789

# 3) the connector — dials the mesh, talks to the daemon, mints the manifest's caps
HATCHER_CONNECTOR_TOKEN=hatcher-connector-demo-auth-token \
COVENANT_OPERATOR_TOKEN=$TOKEN \
COVENANT_DAEMON_URL=http://127.0.0.1:8421 \
HATCHER_MESH_URL=ws://127.0.0.1:8788 \
HATCHER_MANIFEST=examples/repo-doctor.manifest.json \
  pnpm start &                                   # health: http://127.0.0.1:8790/healthz

# drive a dispatch BY HAND through the mesh control plane:
curl -s http://127.0.0.1:8789/status
curl -s -X POST http://127.0.0.1:8789/dispatch \
  -H 'content-type: application/json' \
  -d '{"text":"Inspect this repo, run the test suite, and report."}'
```

`HATCHER_CONNECTOR_TOKEN` must equal the mesh's `MESH_AUTH`: the connector presents it
in the in-band `hello` frame and the mesh rejects a mismatch. The `/dispatch` response
is the `covenant.connector-trace.v0` proof — result + audit root + declared policy.

> Capability note: with the operator token the minted grants don't actually constrain
> (the operator is over-privileged). For real least-privilege, mint a scoped connector
> peer and pass it as `COVENANT_CONNECTOR_TOKEN`. See handoff §§2,5.
