# Covenant Compute Broker

Attestation and lease-lifecycle service for DePIN-backed compute tasks. The broker reserves
compute with a supported provider, signs lease attestations for on-chain bond posting, and
drives the off-chain activation, reclaim, and expiry-sweep paths needed by the compute-bond
roadmap.

## Run

```bash
pnpm --filter @covenant/compute-broker build && pnpm --filter @covenant/compute-broker start
```

## Status

M2 (research) implementation: request/cancel/activate/reclaim/status/expiry-sweep endpoints
are live. Production provider partnerships and on-chain compute-bond enforcement are pending.

## HTTP surface

| Endpoint | Auth | Notes |
|---|---|---|
| `POST /bonds/request` | none | Reserves provider capacity and signs a broker attestation |
| `POST /bonds/cancel` | agent-signed payload | Body: `{ lease_id, agent_did, signed_request, nonce, expires_at }`. Server rejects expired or over-long `expires_at` (cap `BOND_CANCEL_MAX_EXPIRY_SECS`, default 300s) and replayed `nonce` values. The signed message is `JSON.stringify({ action: 'cancel', lease_id, agent_did, nonce, expires_at })`. |
| `POST /leases/activate` | `Authorization: Bearer ${OPERATOR_BEARER_TOKEN}` | Operator-only side-effect. 401 missing, 403 wrong, 503 if `OPERATOR_BEARER_TOKEN` is unset. |
| `POST /leases/reclaim` | operator bearer | same |
| `POST /leases/expire-sweep` | operator bearer | same |
| `GET /leases/:id` | none | Provider status query |
| `GET /healthz` | none | Includes `broker_key_loaded` + `operator_bearer_loaded` flags |
| `GET /metrics` | none | Prometheus |

## Replay-protection scope

The `/bonds/cancel` nonce cache is in-process. Horizontal scaling across multiple
broker instances needs a shared store (Redis is the natural fit; the same
constraint applies to the proof-gen in-memory rate limit). For v0 single-instance
deploys this is sufficient — pre-mainnet posture.
