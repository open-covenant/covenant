# @covenant/proof-gen

Off-chain Groth16 prover for Covenant research and staging circuits. Agents POST a witness and receive a `(proof, public_signals)` blob that can be used for circuit development, artifact validation, and non-mainnet testing.

See [`docs/specs/service-model.md`](../../docs/specs/service-model.md) for service conventions and public payload shapes.

## Run

```
cp .env.example .env
pnpm --filter @covenant/proof-gen build
pnpm --filter @covenant/proof-gen start           # fastify api
pnpm --filter @covenant/proof-gen start:worker    # bullmq worker
```

API and worker are separate processes so they scale independently. Both need the same `REDIS_URL` and `CIRCUIT_ARTIFACTS_DIR`.

## Endpoints

- `POST /prove` — bearer-authenticated JSON body (see `src/schema.ts`). 202 with `{ job_id }`, 503 if circuit artifacts aren't built yet.
- `GET /jobs/:id` — `queued | active | completed | failed`. Completed jobs include the raw Groth16 proof JSON plus `proof_hex` and `public_input_words` for debugging and staging integrations.
- `GET /healthz` — liveness + artifact presence.

## Known pre-mainnet limitations

- Circuit artifacts must be generated before `POST /prove` can accept jobs.
- The API and worker share Redis-backed queue state; deploy them with the same `REDIS_URL`.
- The current `task_completion` circuit is not the canonical mainnet settlement proof. Its public-signal shape is research-only and should not be wired directly to protocol settlement.
- Mainnet zk readiness depends on a settlement-specific circuit, a real ceremony, pinned artifact provenance, and an explicit verifier cutover.

## Artifacts

By default the service looks for artifacts under `services/proof-gen/artifacts/task_completion/build`. You can override that with `CIRCUIT_ARTIFACTS_DIR`.

Expects `task_completion.wasm` + `task_completion.zkey` under `CIRCUIT_ARTIFACTS_DIR`. Until those artifacts are present, the service boots but every `POST /prove` returns 503 `no_artifacts`. `GET /healthz` reports `"artifacts": "missing"`.
