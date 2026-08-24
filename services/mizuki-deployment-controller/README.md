# Mizuki Deployment Controller

This private service implements the updater deployment-hook contract for exactly one shadow service and one production service. Every route requires one bearer credential. Both Render targets must be image-backed, have automatic deploys disabled, and point at the configured registry repository.

The reviewed artifact is an OCI image manifest, not a source archive. Its declared SHA-256 is used as the immutable image digest in `registry/repository@sha256:<digest>`. The controller streams and validates the manifest bytes, triggers Render with that exact digest, and accepts a receipt only when Render reports the same `deploy.image.ref` and `deploy.image.sha`. A Git-backed service, a mutable image tag, a source archive, or a deploy receipt without image evidence fails closed with `artifact_execution_unbound` or `artifact_execution_mismatch`.

This distinction is load-bearing: downloading an archive and separately asking Render to build a Git commit does not prove that Render executed the reviewed bytes. The existing release pipeline must publish the reviewed build as an OCI image and supply its manifest bytes, digest, and length before this controller can admit it.

## Functional readiness contract

Shadow is deliberately zero-authority. The controller probes its private, tokenless deployment endpoint:

```text
GET /deployz
```

The only accepted body is strict JSON `{ "ok": true }`. The runtime returns it only after reading its isolated database and confirming both durable admission controls are closed. A response with any additional field fails the probe. Shadow has no production probe token, provider credential, coding-gateway access, signer access, job authority, GitHub App, updater access, or live payment configuration.

Production exposes an authenticated application dependency probe:

```text
GET /internal/mizuki/functional-readiness
Authorization: Bearer $MIZUKI_DEPLOY_PRODUCTION_PROBE_TOKEN
```

The response is strict JSON:

```json
{
  "status": "ok",
  "service": "mizuki-api",
  "checks": {
    "database": "ok",
    "policySigner": "ok",
    "codingGateway": "ok",
    "settlement": "ok"
  }
}
```

This probe checks every runtime dependency except the updater. The operator-facing runtime
`/readyz` still requires the updater, while updater readiness requires this controller. Keeping the
deployment probe independent of the updater makes the steady-state readiness graph acyclic without
weakening paid-job admission or operator readiness.

The application does not self-report its image digest. The controller reads Render's sole live deploy ID and immutable image digest immediately before and after the functional probe. Any change during that interval invalidates the probe.

## State and recovery

The Postgres operation record and its append-only event are committed in one transaction. Database triggers reject event updates and deletes. The controller writes a `triggering` event before every Render mutation. If a response is lost, it reconciles one exact service, digest, trigger, and time window. It never repeats an uncertain mutation; no match after the grace interval and multiple matches both require operator reconciliation.

Stable idempotency keys are checked before the artifact is downloaded. Render service identity, type, image repository, region, runtime, suspension state, and automatic-deploy state are fingerprinted and checked again immediately before mutation. Expected in-flight operations are included in controller readiness instead of being mistaken for unrelated service instability.

An unhealthy shadow is restored to its exact recorded baseline before its reservation is released. Promotion first restores shadow, records the exact production baseline, and deploys the reviewed image digest. The single production slot remains owned until the updater explicitly finalizes the exact active, healthy promotion after the minimum soak age, or rollback restores the baseline. Elapsed time alone never releases the slot. Finalization is idempotent and rechecks immutable Render evidence plus the functional probe. Rollback can recover a lost promotion receipt from independent Render evidence even when the caller lacks the promotion ID. Rollback completes only after the exact baseline digest is live and passes the functional probe.

## Routes

All routes require `Authorization: Bearer $MIZUKI_DEPLOY_AUTH_TOKEN`.

- `GET /healthz` checks Postgres without external mutations.
- `GET /readyz` validates Postgres, both Render targets, functional application probes, and any active operation evidence.
- `POST /v1/deployments/shadow`
- `GET /v1/deployments/shadow/:deploymentId/health`
- `POST /v1/deployments/promote`
- `GET /v1/deployments/production/:deploymentId/health`
- `POST /v1/deployments/finalize`
- `POST /v1/deployments/rollback`

The POST schemas are the strict version-1 schemas in `src/domain.ts`. Unknown fields, mismatched operation keys, non-allowlisted repositories, alternate service IDs, alternate API origins, and unapproved artifact origins are rejected. Retryable responses include `Retry-After`; unresolved or ambiguous mutation evidence returns a non-retryable conflict.

## Required deployment shape

- Dedicated Postgres database with an explicit TLS mode and connection timeout.
- Two image-backed Render services using the same configured registry repository.
- Shadow must be a private service; production must be a web service.
- Automatic deploys disabled on both targets.
- Exact private shadow `/deployz` and HTTPS production functional-readiness URLs.
- Separate controller and production-only application-probe bearer credentials; shadow receives neither.
- Artifact origins restricted to the OCI manifest publisher or an immutable mirror.

Render API keys are account-wide credentials, not workspace-scoped. Exact service allowlists reduce what this controller will mutate, but they do not reduce the source credential's broader blast radius. Use a dedicated Render account identity with the least account access available and rotate the key if the controller is compromised.

## Run

```bash
pnpm install --ignore-workspace --frozen-lockfile
pnpm test
pnpm typecheck
pnpm build
pnpm start
```
