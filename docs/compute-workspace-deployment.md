# Encrypted workspace deployment

This runbook prepares adoption package 3. The implementation is locally
qualified and R2 provider acceptance passed; deployment, copied-state recovery,
installed-client acceptance and a real cross-lease demonstration remain pending. See the separate
[evidence record](compute-workspace-release-evidence.md).

## Existing service and storage

Use the existing Compute coordinator service and persistent disk. Workspace
metadata goes in `workspaces.jsonl` beside its existing `journal.jsonl`; no new
Compute service or disk is required. Do not alter settlement, payout, staking or
other service configuration as part of this release.

The approved provider is Covenant's Cloudflare R2 account. Enrollment is complete
and the dedicated private Standard bucket `covenant-compute-workspaces` exists,
with bucket-scoped object credentials. Keep public
development URLs and custom-domain exposure disabled. Store credentials only in
the private credential store and the coordinator's deployment secrets.

Configure the following, preserving all unrelated environment variables:

```text
COVENANT_COMPUTE_WORKSPACE_S3_ENDPOINT=<verified account R2 S3 endpoint>
COVENANT_COMPUTE_WORKSPACE_S3_REGION=auto
COVENANT_COMPUTE_WORKSPACE_S3_BUCKET=covenant-compute-workspaces
COVENANT_COMPUTE_WORKSPACE_S3_PREFIX=covenant-workspaces
COVENANT_COMPUTE_WORKSPACE_S3_ACCESS_KEY=<bucket-scoped secret>
COVENANT_COMPUTE_WORKSPACE_S3_SECRET_KEY=<bucket-scoped secret>
COVENANT_COMPUTE_WORKSPACE_MAX_BYTES=1073741824
COVENANT_COMPUTE_WORKSPACE_MAX_OWNERS=1024
COVENANT_COMPUTE_WORKSPACE_WRITES_ENABLED=false
```

The 1 GiB ceiling counts committed ciphertext, pending uploads and garbage.
Issuance/HEAD limits are 256 per owner and 4096 globally per UTC day. Valid
signed URLs can be reused until expiry, so these are application limits, not a
hard provider bill cap. Do not set a session token unless the provider issued
one. Confirm the endpoint and region from the actual account.

## Provider acceptance

From `agent-os`, load the provider environment privately and run:

```sh
cargo run --locked -p covenant-compute-coordinator \
  --example workspace-storage-acceptance -- --allow-synthetic-storage-writes
```

This writes two tiny synthetic keys and records a sanitized JSON verdict.
Require duplicate conditional PUT rejection, corrupt checksum rejection,
provider-verified HEAD checksum and size, anonymous read rejection, exact GET
bytes, expired-grant rejection and deletion. Unknown write outcomes retain
cleanup keys until every possible write grant expires; reconcile them before
calling cleanup complete. Missing verified-checksum support blocks release;
ETag or caller-written metadata is not a substitute.

## Additive state and recovery

Privately snapshot existing production state and record financial metrics before
promotion. First workspace startup initializes a separate journal and lockfile.
Existing jobs, balances and lease records must remain unchanged by this addition.
The local additive-replay check is:

```sh
cargo test --locked -p covenant-compute-coordinator --lib \
  workspace::tests::compaction_and_additive_journal_do_not_rewrite_existing_compute_state -- --exact
```

That check uses synthetic state. It does not establish recovery of a production
copy. After the first synthetic provider checkpoint, copy the workspace journal
privately and run the workspace-only harness:

```sh
cargo run --locked -p covenant-compute-coordinator \
  --example workspace-state-rehearsal -- \
  --copied-journal /private/copy/workspaces.jsonl
```

The source file is read only. Actual workspace replay/compaction opens a private
temporary copy twice and a separate copy with an interrupted trailing record.
It compares complete committed views, pending uploads, tombstones, counters and
cleanup obligations, then reports aggregate counts and the source digest without
owner identifiers, names, keys or object paths. Its object adapter refuses every
operation; it constructs an in-memory coordinator without starting any worker.
The input has a 256 MiB rehearsal ceiling. Keep the source snapshot private and
untouched. Rehearsal against an actual copied production checkpoint is pending.

Do not launch the full coordinator against copied financial state with
production provider, payout or chain credentials: startup workers can perform
external actions. A workspace-only harness must construct an isolated in-memory
coordinator and must not run cleanup or financial workers.

## Build and promote

Reconcile the focused branch with the latest deployment branch before review.
Do not merge the broad parity branch. The existing
`.github/workflows/compute-coordinator-image.yml` publishes immutable commit tags
for the coordinator on `deploy/compute-coordinator`. Resolve the public image
digest, verify anonymous layer downloads and the embedded source revision, then
prepare one controlled deployment on the existing service. Record the previous
revision and image before submission; reconcile an unknown response by reading
deployment state rather than resubmitting blindly.

The CLI release workflow is `.github/workflows/compute-release.yml`. A new
`compute-v*` tag must match the packaged commands' versions and have matching
release notes. `deploy/compute-release/dist.sh` builds and checks four macOS/Linux
archives. This candidate prepares 0.3.0 versions, lockfiles and release notes;
the existing 0.2.0 assets remain immutable. Do not overwrite an existing release.
Verify downloaded signatures, provenance,
checksums, version and workspace help before installed-client acceptance.

Deploy the accepted coordinator with workspace writes disabled. Confirm its
exact `/health` revision, existing status and journal health. Enable new writes
only after provider qualification. Using the released CLI, save a small synthetic
checkpoint, restart the buyer process, restore and compare its exact digest,
export/import private state under the same buyer identity, exercise invalid
owner/version controls, delete and confirm cleanup. Preserve the same upload ID
and ciphertext for an uncertain retry; a new save must not bypass it.

## Cross-lease acceptance

The release owner allocates this demonstration from the remaining shared test
budget. On 2026-10-18, public capacity advertised an L40S with 48 GB VRAM at
1.20 USDC/hour. Two five-minute windows at a maximum 334 micro-USDC/second would
reserve 0.2004 USDC total. This is a planning estimate, not a current quote or
availability guarantee. Refresh the quote and enforce the ceiling before any
spend; separately account for backing-provider charges and network fees.

1. Rent a bounded first lease and verify SSH host identity through an
   independently authenticated source. Create a synthetic checkpoint archive,
   record its machine and plaintext hash, and copy it to the buyer via SSH.
2. Encrypt/save the archive with the released CLI. Close the first lease and
   verify settlement, refund and provider deletion before starting the next.
3. Rent a distinct later lease, verify its host identity, restore with `--lease`,
   transfer the archive via authenticated SSH and verify its hash on that machine.
   Record the ciphertext hash separately from the plaintext hash.
4. Close and settle the later lease, confirm machine deletion, delete the
   synthetic workspace and verify object cleanup after grants expire.

`--lease` authorizes restoration; it does not transfer plaintext to the machine.
The release accepts checkpoint files and explicit SSH transfers, not automatic
directory synchronization. Hardware trust remains `open`. An unauthenticated
`ssh-keyscan` alone cannot satisfy the host-identity gate.

## Rollback and announcement

Set `COVENANT_COMPUTE_WORKSPACE_WRITES_ENABLED=false` to stop new admissions while
retaining the workspace-capable binary, storage configuration, commit recovery,
owner reads and cleanup. Removing the bucket/configuration or reverting to a
pre-workspace image is not a routine rollback while obligations remain.

After provider, recovery, installed-client, deployment, cross-lease and cleanup
acceptance, publish the page and sharing image, verify their live revision and X
metadata, then prepare the launch post. Until then, describe this as a release
candidate. Do not claim automatic synchronization, attested hardware or restored
training progress without separate evidence.
