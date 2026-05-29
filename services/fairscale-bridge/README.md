# fairscale-bridge

Read-only HTTP endpoint that exposes Covenant's audit-attested conduct events
for FairScale to pull into the `work_history` pillar of agent-score.

It stores nothing. On each request it reads the daemon's hash-chained audit log
over the private network (`COVENANT_DAEMON_URL`, operator token), maps each
`AuditEvent` into a FairScale conduct event, and returns the page alongside the
daemon's verified Merkle root so the consumer can confirm integrity.

## Auth

FairScale presents `Authorization: Bearer <FAIRSCALE_API_TOKEN>`, compared in
constant time. `/healthz` is the only unauthenticated route. Authed responses
are `Cache-Control: no-store`.

## Endpoints

- `GET /healthz` — liveness + daemon reachability (public).
- `GET /v1/conduct-events?since=<ms|iso>&cursor=<token>&limit=<n>` — conduct
  events across all agents the daemon exposes, oldest first.
- `GET /v1/agents/:agentId/conduct-events?...` — same, scoped to one agent
  (`agentId` matches the base58 pubkey or the `name@host` display).
- `GET /v1/attestation` — current audit integrity report (root hash, validity,
  event/anchor counts). Strict: `502` if the daemon can't be verified.

### Pagination

Forward-only. With no `since`/`cursor` you get the stream from the beginning.
`limit` defaults to `FAIRSCALE_BRIDGE_DEFAULT_LIMIT` (100), capped at
`FAIRSCALE_BRIDGE_MAX_LIMIT` (1000).

The response carries an opaque `next_cursor` and `has_more`. Pass `next_cursor`
back as `cursor` to continue — it encodes the exact `(timestamp, id)` position,
so pages never overlap or skip, **no client-side dedupe needed**. When
`has_more` is `false`, `next_cursor` still points at the last event so you can
persist it and poll for new events later. `since` accepts an epoch-ms or ISO
string as a convenience starting point (inclusive of that millisecond).

`truncated: true` means the page was bounded by `FAIRSCALE_BRIDGE_FETCH_CAP`
(the daemon's audit API has no offset, so the bridge pulls events-since and
slices); raise the cap or pull more frequently if you ever see it.

### Attestation

Every page includes `attested` and an `attestation` block (the verified audit
root + counts). Attestation is best-effort on data routes: if the daemon's
verify call fails transiently, events are still returned with `attested: false`
and `attestation: null`. Use `/v1/attestation` when you need a hard integrity
check.

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
  "summary": "intent success",
  "detail": { "...": "structural audit-kind fields; free-text content redacted" }
}
```

Free-text content fields (`intent_text`, approval `choices`, any `*_text`) are
redacted to `[redacted:<len>]` before leaving the boundary — the scoring pillar
gets the signal, not the payload.

## Env

| var | required | default | purpose |
| --- | --- | --- | --- |
| `FAIRSCALE_API_TOKEN` | yes | — | bearer FairScale must present (≥24 chars) |
| `COVENANT_OPERATOR_TOKEN` | yes | — | bearer used to read the daemon |
| `COVENANT_DAEMON_URL` | no | `http://127.0.0.1:8421` | daemon base URL |
| `PORT` / `FAIRSCALE_BRIDGE_PORT` | no | `8788` | listen port (`PORT` wins; Render sets it) |
| `COVENANT_DAEMON_TIMEOUT_MS` | no | `15000` | per-call daemon timeout |
| `COVENANT_DAEMON_RETRIES` | no | `1` | retries on transient daemon errors (network / 5xx) |
| `FAIRSCALE_BRIDGE_DEFAULT_LIMIT` | no | `100` | default page size |
| `FAIRSCALE_BRIDGE_MAX_LIMIT` | no | `1000` | hard page-size cap |
| `FAIRSCALE_BRIDGE_FETCH_CAP` | no | `100000` | max events pulled from the daemon per request |

## Dev

```sh
pnpm install --ignore-workspace --frozen-lockfile=false
pnpm test
pnpm build && pnpm start
```
