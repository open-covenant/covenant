# @covenant/proof-gen

Off-chain Groth16 prover for Covenant research and staging circuits. Agents POST a witness and receive a `(proof, public_signals)` blob that can be used for circuit development, artifact validation, and non-mainnet testing.

## Run

```
cp .env.example .env
pnpm --filter @covenant/proof-gen build
pnpm --filter @covenant/proof-gen start           # fastify api
pnpm --filter @covenant/proof-gen start:worker    # bullmq worker
```

API and worker are separate processes so they scale independently. Both need the same `REDIS_URL`, `CIRCUIT_ARTIFACTS_DIR`, and **`PROOFGEN_WITNESS_WRAP_KEY`** (32-byte base64 secret held outside Redis; both processes refuse to start without it).

## Env

| Var | Required | Default | Notes |
|---|---|---|---|
| `REDIS_URL` | yes | `redis://127.0.0.1:6379` | API and worker share queue + cache state |
| `PROOFGEN_WITNESS_WRAP_KEY` | yes | — | 32-byte base64. Long-lived secret that wraps each job's witness AES key (envelope encryption). Redis-only compromise no longer yields plaintext. |
| `SESSION_SECRET` | yes | — | HS256 JWT verify secret (>=32 chars). Required by `/prove`. |
| `PROOFGEN_RATE_LIMIT_BURST` | no | `10` | Per-agent requests permitted within the window |
| `PROOFGEN_RATE_LIMIT_WINDOW_MS` | no | `60000` | Window length in ms |
| `CIRCUIT_ARTIFACTS_DIR` | no | `./artifacts/task_completion/build` | Where wasm + zkey live |
| `PROOFGEN_PORT` | no | `8787` | API listen port |
| `PROOFGEN_WORKER_HEALTH_PORT` | no | `8786` | Worker process `/healthz` + `/metrics` port |
| `PROOFGEN_WORKER_CONCURRENCY` | no | `1` | BullMQ worker concurrency |
| `PROOFGEN_RESULT_TTL_SEC` | no | `3600` | Job result + cache TTL |
| `LOG_LEVEL` | no | `info` | Pino log level |

## Worker health surface

The worker process exposes its own HTTP listener on `PROOFGEN_WORKER_HEALTH_PORT`
(default `8786`) — distinct from the API on `PROOFGEN_PORT`:

| Endpoint | Returns |
|---|---|
| `GET /healthz` | `{ ok, running, last_processed_at, last_processed_age_ms, concurrency, artifacts_dir }`. 200 when the BullMQ worker is running, 503 otherwise. `last_processed_at` lets a probe detect a wedged consumer that the BullMQ liveness alone wouldn't catch. |
| `GET /metrics` | Same Prometheus registry the API exposes — jobs, durations, cache hits. Scrape both endpoints if running in separate pods. |

## Endpoints

- `POST /prove` — bearer-authenticated JSON body (see `src/schema.ts`). 202 with `{ job_id }`, 503 if circuit artifacts aren't built yet.
- `GET /jobs/:id` — `queued | active | completed | failed`. Completed jobs include the raw Groth16 proof JSON plus `proof_hex` and `public_input_words` for debugging and staging integrations.
- `GET /healthz` — liveness + artifact presence.

## Known pre-mainnet limitations

- Circuit artifacts must be generated before `POST /prove` can accept jobs.
- The API and worker share Redis-backed queue state; deploy them with the same `REDIS_URL` and `PROOFGEN_WITNESS_WRAP_KEY`.
- The current `task_completion` circuit is not the canonical mainnet settlement proof. Its public-signal shape is research-only and should not be wired directly to protocol settlement.
- Mainnet zk readiness depends on a settlement-specific circuit, a real ceremony, pinned artifact provenance, and an explicit verifier cutover.
- The Redis rate limit and witness envelope are both v0 single-tier: rotation of `PROOFGEN_WITNESS_WRAP_KEY` invalidates in-flight jobs encrypted under the old key. A future iteration should support a key-id field in the wrapped blob for graceful rotation.

## Artifacts

By default the service looks for artifacts under `services/proof-gen/artifacts/task_completion/build`. You can override that with `CIRCUIT_ARTIFACTS_DIR`.

Expects `task_completion.wasm` + `task_completion.zkey` under `CIRCUIT_ARTIFACTS_DIR`. Until those artifacts are present, the service boots but every `POST /prove` returns 503 `no_artifacts`. `GET /healthz` reports `"artifacts": "missing"`.
