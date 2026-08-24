# Mizuki Updater

Mizuki Updater applies signed, benchmarked, independently reviewed changes to Mizuki. It verifies the release evidence, synchronizes a GitHub pull request, waits for every named check, exercises the candidate in a shadow deployment, merges the exact reviewed commit, promotes it, and observes the promoted deployment through a continuous health soak. Once a shadow exists, any terminal failure invokes rollback.

This service has no wallet keys, financial credentials, transfer methods, or payment endpoints. Mizuki can change his application, but this updater can only operate on explicitly allowed repositories and fixed deployment hooks.

## Release contract

An upgrade moves through this durable state machine:

```text
submitted -> verifying_artifact -> proposal_verified -> syncing_pr
          -> waiting_checks -> starting_shadow -> checking_shadow
          -> merging -> merge_triggering -> promoting
          -> verifying_promotion -> completed
```

Before a shadow deployment, a terminal failure ends at `failed`. After a shadow deployment, a terminal failure moves through `rollback_pending` to `rolled_back`. A failed rollback ends at `rollback_failed` and requires operator intervention.

The updater fails closed unless all of the following are true:

- The manifest SHA-256 matches canonical manifest JSON.
- A trusted Ed25519 key signed the manifest hash.
- The manifest carries the lowercase SHA-256 of the exact public capability handoff that sourced the external proposal.
- The proposal, benchmark, and review receipts are fresh.
- Both receipt SHA-256 values match their canonical JSON and their trusted Ed25519 attestations.
- Both receipts bind the candidate commit and artifact.
- The benchmark clears its declared improvement threshold and its protected suite passed.
- The reviewer route differs from the implementation route.
- The streamed artifact has the exact declared byte length and SHA-256.
- The repository, base branch, candidate-branch prefix, and mandatory check set satisfy immutable deployment configuration.
- The signed base and candidate revisions still match both branch refs.
- Every required GitHub Actions check is successful and belongs to its configured workflow run.
- Shadow health reports the reviewed commit as healthy.
- The required checks are still successful immediately before merge.
- The durable promotion control is explicitly enabled before the updater acquires the single active promotion reservation.
- That reservation remains owned through merge reconciliation, production promotion, the full health soak, controller finalization, and any terminal rollback.
- The promoted deployment reports the exact reviewed commit as continuously healthy for the configured soak window.

External actions are resumable. Pull-request synchronization and merge inspect existing GitHub state before acting, including reconciliation of a merge committed before a worker crash. Deployment hooks receive stable idempotency keys and must return the same receipt when those keys are replayed. Promotion hook success is durably recorded as `verifying_promotion`; it is never treated as completion by itself. Completion requires a successful controller finalization after the soak. Promotion control starts closed after migration. Every control change, reservation acquisition, release, terminal reconciliation, rollback lockout, and operator resolution advances its revision and appends an immutable audit entry. A failed rollback retains its reservation and closes promotions until an operator explicitly resolves it while control remains closed.

## Signed proposal

`POST /v1/upgrades` accepts this strict envelope:

```json
{
  "keyId": "release-key-v1",
  "manifest": {
    "version": 1,
    "proposalId": "upgrade-2026-08-22-1",
    "sourceHandoffSha256": "<handoffSha256 from GET /v1/capabilities/:capabilityId/handoff>",
    "repository": {
      "owner": "open-covenant",
      "name": "covenant",
      "baseBranch": "main",
      "baseSha": "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
      "headBranch": "mizuki/capability/upgrade-2026-08-22-1"
    },
    "candidateSha": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
    "artifact": {
      "url": "https://artifacts.example.com/upgrade.tar.gz",
      "sha256": "<64 lowercase hex characters>",
      "sizeBytes": 12345
    },
    "title": "improve maintenance reliability",
    "body": "Raises the protected maintenance benchmark.",
    "requiredChecks": ["test", "security"],
    "benchmark": {
      "receipt": {
        "version": 1,
        "receiptId": "benchmark-1",
        "candidateSha": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        "artifactSha256": "<artifact SHA-256>",
        "suite": "maintenance-reliability",
        "targetMetric": "successful-jobs",
        "direction": "increase",
        "baseline": 80,
        "candidate": 95,
        "minimumImprovement": 10,
        "protectedSuitePassed": true,
        "completedAt": "2026-08-22T12:00:00.000Z"
      },
      "sha256": "<SHA-256 of canonical benchmark receipt JSON>",
      "keyId": "benchmark-key-v1",
      "signature": "<base64 Ed25519 benchmark signature>"
    },
    "review": {
      "receipt": {
        "version": 1,
        "receiptId": "review-1",
        "candidateSha": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        "artifactSha256": "<artifact SHA-256>",
        "implementerRoute": "provider/implementer",
        "reviewerRoute": "provider/reviewer",
        "verdict": "approved",
        "blockingFindings": 0,
        "summary": "Candidate satisfies the upgrade contract.",
        "completedAt": "2026-08-22T12:00:00.000Z"
      },
      "sha256": "<SHA-256 of canonical review receipt JSON>",
      "keyId": "review-key-v1",
      "signature": "<base64 Ed25519 review signature>"
    },
    "issuedAt": "2026-08-22T12:00:00.000Z"
  },
  "manifestSha256": "<SHA-256 of canonical manifest JSON>",
  "signature": "<base64 Ed25519 signature>"
}
```

Canonical JSON recursively sorts object keys, preserves array order, and uses standard JSON primitives without whitespace. Sign these UTF-8 bytes:

```text
mizuki-upgrade-v1:<keyId>:<manifestSha256>
```

Hash each receipt before inserting it into the manifest, hash the complete manifest, then sign. The proposal-signing private key must not be present in the updater deployment.

The benchmark signer signs `mizuki-benchmark-v1:<keyId>:<receiptSha256>`. The independent reviewer signs `mizuki-review-v1:<keyId>:<receiptSha256>`. Proposal, benchmark, and review public keys must resolve to three distinct Ed25519 keys. Their private keys belong in separate execution contexts and never in this service.

## API

All routes except `GET /health` require bearer authentication. `MIZUKI_UPDATER_SUBMIT_TOKEN` authorizes proposal submission only. `MIZUKI_UPDATER_CONTROL_TOKEN` authorizes promotion-control mutation and failed-rollback resolution only. `MIZUKI_UPDATER_READ_TOKEN` authorizes reads only and is the only updater credential given to the Mizuki core. All three tokens and the deployment-hook token must be distinct.

- `POST /v1/upgrades` requires `Content-Type: application/json` and an `Idempotency-Key` header. It returns `202` after durable reservation.
- `GET /v1/upgrades/:id` returns current state and public external receipts.
- `GET /v1/proposals/:proposalId` resolves the durable upgrade for a signed manifest proposal ID and returns its audit-head hash. Mizuki uses this read-only route to reconcile its public capability record.
- `GET /v1/upgrades/:id/audit` returns the ordered, SHA-256-linked audit chain.
- `GET /v1/admin/promotion-control` returns the durable promotion state, reservation owner, revision, reason, authority role, and update time. It requires the read token.
- `GET /v1/admin/promotion-control/audit` returns the append-only ordered control and reservation ledger. It requires the read token.
- `PUT /v1/admin/promotion-control` requires the control token and strict JSON with `promotionsEnabled`, `expectedRevision`, and an operator reason. A stale revision returns `409`.
- `POST /v1/admin/promotion-control/resolve-failure` requires the control token, a matching failed upgrade ID and revision, and an operator reason. It clears only the failed reservation; promotions remain disabled until a separate revisioned enable request.
- `GET /metrics` returns Prometheus metrics.
- `GET /health` checks durable storage and is safe for a platform health probe.

The request limit is 128 KiB. Unknown fields are rejected at every proposal level.

Example pause body:

```json
{
  "promotionsEnabled": false,
  "expectedRevision": 4,
  "reason": "incident response: stop new production promotions"
}
```

Control mutation and reservation ownership use the same database advisory gate. The updater acquires one durable reservation before entering `merge_triggering` and does not release it merely because a lease, process, or deploy call ends. A successful pause prevents a new reservation but does not cancel the current owner mid-release. Production-health checks, controller finalization, and rollback continue for the owner while control is paused.

For a core-generated capability proposal, the signed manifest must use that core upgrade UUID as `proposalId` and copy the handoff's `handoffSha256` into `sourceHandoffSha256` before hashing and signing the manifest. The updater exposes that signed source hash on both upgrade read routes. The core recomputes the current deterministic handoff from the capability, upgrade trigger, and ordered failure evidence and refuses to advance unless the hashes match exactly. New failure evidence invalidates an older handoff. Only the external proposal authority submits the envelope; the core has no submission or signing path.

## Deployment hook contract

The updater authenticates every hook with its configured bearer token. POST hooks also receive `Idempotency-Key`. `MIZUKI_UPDATER_DEPLOY_CONTROLLER_HOSTPORT` is the only configurable network value: the updater constructs every fixed controller path itself. Production accepts only Render's private `http://mizuki-deployment-controller:8794` origin, so a receipt or environment value cannot redirect the credential.

Shadow creation receives the repository, candidate, artifact, pull request, and manifest receipt. It must return:

```json
{ "deploymentId": "shadow-1" }
```

The deployment system must fetch the artifact from the supplied URL and independently enforce its supplied SHA-256 and byte length before executing it. The URL alone is not an artifact identity.

The configured shadow-health URL replaces `{deploymentId}` with the URL-encoded ID. Every response must declare `environment: "shadow"` and return one of:

```json
{
  "status": "starting",
  "candidateSha": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
  "environment": "shadow"
}
```

```json
{
  "status": "healthy",
  "candidateSha": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
  "environment": "shadow"
}
```

```json
{
  "status": "unhealthy",
  "candidateSha": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
  "environment": "shadow",
  "detail": "health-check explanation"
}
```

Promotion must finish activation and return a stable receipt:

```json
{ "status": "completed", "operationId": "promotion-1" }
```

The updater persists that operation ID before polling the controller's fixed production-health path. Production health must bind the active route to the candidate, merge, and promotion operation:

```json
{
  "status": "healthy",
  "candidateSha": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
  "environment": "production",
  "active": true,
  "mergeSha": "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
  "promotionOperationId": "promotion-1"
}
```

`healthy` with `active: false`, a shadow-environment receipt, or a mismatched candidate, merge, or operation is rejected and rolled back. `starting` is allowed only before the first active healthy observation. `unhealthy`, a return to `starting` after health was established, exhausted health retries, or `MIZUKI_UPDATER_PROMOTION_TIMEOUT_MS` also triggers rollback. Only a latest active production receipt at the end of `MIZUKI_UPDATER_PROMOTION_SOAK_MS` moves the upgrade to `completed`.

The rollback hook receives the persisted promotion operation ID when promotion began. It must finish rollback and return `{ "status": "completed" }`, with an optional rollback `operationId`. Asynchronous acceptance responses are rejected.

## GitHub App permissions

Install the configured GitHub App only on approved repositories. It needs:

- Actions: read
- Metadata: read
- Contents: write
- Pull requests: write
- Checks: read

Readiness authenticates the configured App, validates every allowlisted installation, and resolves every pinned workflow ID to its exact active path through GitHub's Actions API. Each operation requests a short-lived token for exactly one repository with this exact permission map, then rejects all-repository selection, a missing or additional permission, a different repository, an invalid lifetime, a suspended installation, or App identity drift.

`MIZUKI_UPDATER_CHECK_PRODUCERS_JSON` pins each accepted context to one check-run App ID, numeric workflow ID, workflow path, `pull_request` event, manifest head branch, candidate head SHA, manifest base branch, signed base SHA, and the workflow definition at that signed base. Check admission joins GitHub's check-run and Actions workflow-run responses through the check-suite ID, then verifies the exact repository, head repository, pull request, refs, and commits. A same-name result from another App, workflow, event, repository, pull request, ref, or commit fails the entire context.

Before trusting a run, the updater compares the pinned workflow file's Git blob identity at the signed base commit and candidate commit. A candidate cannot weaken or replace the checks that evaluate it. The production policy binds `application` and `escrow` to workflow `340541049` at `.github/workflows/mizuki.yml`, and `rust` and `landing` to workflow `265742803` at `.github/workflows/ci.yml`. Both use GitHub Actions App ID `15368`; all semantic selectors are fixed to the signed manifest contract.

The signed manifest binds both the base branch and its exact SHA. The updater verifies the current base and candidate refs before check admission and again immediately before an unmerged PR can merge. A protected base advance fails closed and requires a newly reviewed, re-signed proposal. The merge request also supplies the expected candidate SHA.
The App must not have branch-protection bypass rights; GitHub branch rules are the final protection against a check changing between the updater's last poll and the merge transaction.

`MIZUKI_UPDATER_MANDATORY_CHECKS` is an operator-controlled floor. A signed manifest may add checks but cannot omit these checks, and an added check fails until the operator pins its producer separately. Base branches and the candidate branch prefix are similarly constrained outside Mizuki's mutable application.

## Run

```bash
cp .env.example .env
pnpm install --ignore-workspace --frozen-lockfile
pnpm test
pnpm build
set -a && . ./.env && set +a
pnpm start
```

Set `MIZUKI_UPDATER_TEST_DATABASE_URL` to a dedicated PostgreSQL test database to run the real migration test. It creates and removes an isolated schema, concurrently starts two migrators, replays the migration, and exercises the prior state constraint upgrade.

The service applies checksum-verified Postgres migrations at startup inside one transaction protected by an advisory lock. Migration history is durable in `mizuki_updater_migrations`; a missing, unknown, or changed migration fails startup. The migration creates one closed promotion-control row at revision zero. Missing control state fails promotion admission closed. `MIZUKI_UPDATER_MEMORY_STORE=true` is restricted to a non-production loopback listener.

The event Blueprint deliberately runs one updater instance and relies on durable Postgres leases plus restart recovery. Do not increase the instance count during the event without first exercising concurrent workers against the exact production database and deployment hooks. Leases and optimistic versions prevent two tested workers from advancing the same upgrade when horizontal redundancy is enabled later.

## Operations

- Alert on `rollback_failed`, any nonzero `mizuki_updater_errors_total` increase, and upgrades stuck beyond their configured check, shadow-health, or promotion-health deadline.
- Keep the promotion timeout at least one poll interval longer than the soak. The event Blueprint uses a three-hour soak, a three-hour-ten-minute deadline, and five-second polls.
- Preserve audit rows indefinitely. Each receipt links the preceding hash, so deletion or reordering is detectable.
- Rotate the API token, deployment token, and GitHub App key independently.
- Rotate proposal keys by adding the new public key, deploying, changing the offline signer, then removing the old key after all old proposals expire.
- Keep promotions closed except during an observed release window. Read the current revision, enable with a recorded reason, watch the candidate through the production soak, then close the next revision.
- If a pause request times out while a hook may be in flight, keep the control closed, read the upgrade record, and reconcile the hook's stable idempotency key with the deployment system. Do not reopen or manually repeat promotion until the operation ID and active production revision are known.
- Investigate a failed rollback before any further promotion. Do not edit an upgrade row to bypass a gate.
