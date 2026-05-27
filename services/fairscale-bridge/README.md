# fairscale-bridge

Read-only HTTP endpoint that exposes Covenant's audit-attested conduct events
for FairScale to pull into the `work_history` pillar of agent-score.

It does not store anything. On each request it reads the daemon's hash-chained
audit log over the private network (`COVENANT_DAEMON_URL`, operator token), maps
each `AuditEvent` into a FairScale conduct event, and returns the page alongside
the daemon's verified Merkle root so the consumer can confirm integrity.

## Auth

FairScale presents `Authorization: Bearer <FAIRSCALE_API_TOKEN>`. Compared in
constant time. `/healthz` is the only unauthenticated route.

## Endpoints

- `GET /healthz` — liveness + daemon reachability.
- `GET /v1/conduct-events?since=<ms|iso>&limit=<n>&cursor=<ms>` — conduct events
  across all agents the daemon exposes, oldest first.
- `GET /v1/agents/:agentId/conduct-events?...` — same, scoped to one agent
  (`agentId` matches the base58 pubkey or the `name@host` display).
- `GET /v1/attestation` — current audit integrity report (root hash, validity,
  event/anchor counts).

### Pagination

`limit` defaults to `FAIRSCALE_BRIDGE_DEFAULT_LIMIT` (100), capped at
`FAIRSCALE_BRIDGE_MAX_LIMIT` (1000). The response carries `next_cursor` (the
last event's epoch-ms) and `has_more`. Pass `next_cursor` back as `cursor` to
continue; it is inclusive, so dedupe by the stable `id`.

### Conduct event shape

```json
{
  "id": "uuid",
  "agent_id": "<base58 pubkey>",
  "agent_display": "name@host",
  "occurred_at": "2025-01-01T00:00:00.000Z",
  "occurred_at_ms": 1735689600000,
  "source": "covenant",
  "pillar": "work_history",
  "event_type": "intent_dispatched",
  "outcome": "success",
  "weight": 3,
  "summary": "intent success: ...",
  "detail": { "...": "remaining audit-kind fields" }
}
```

## Env

| var | required | default | purpose |
| --- | --- | --- | --- |
| `FAIRSCALE_API_TOKEN` | yes | — | bearer FairScale must present (≥24 chars) |
| `COVENANT_OPERATOR_TOKEN` | yes | — | bearer used to read the daemon |
| `COVENANT_DAEMON_URL` | no | `http://127.0.0.1:8421` | daemon base URL |
| `PORT` / `FAIRSCALE_BRIDGE_PORT` | no | `8788` | listen port (`PORT` wins; Render sets it) |
| `FAIRSCALE_BRIDGE_DEFAULT_LIMIT` | no | `100` | default page size |
| `FAIRSCALE_BRIDGE_MAX_LIMIT` | no | `1000` | hard page-size cap |

## Dev

```sh
pnpm install --ignore-workspace --frozen-lockfile=false
pnpm test
pnpm build && pnpm start
```
