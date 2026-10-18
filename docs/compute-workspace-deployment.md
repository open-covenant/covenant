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
The 4096 lifetime object-allocation cap also counts permanent empty fences;
restart and workspace deletion do not reset it.
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

This writes three tiny synthetic keys and records a sanitized JSON verdict.
Require duplicate conditional PUT rejection, corrupt checksum rejection,
provider-verified HEAD checksum and size, anonymous read rejection, exact GET
bytes, expired-grant rejection and cleanup. The delayed-body test must complete
a conditional PUT only after an empty fence is installed, receive 412 and verify
the empty checksum/size. Unknown write outcomes retain full cleanup obligations;
expiry alone does not prove completion. Production removes encrypted payloads
using permanent private empty fences and must never delete those fences. Missing verified-checksum support blocks release;
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

The approved acceptance allocation uses two 1200-second leases at a ceiling of
334 micro-USDC/second: 0.4008 USDC per lease and 0.8016 USDC in total purchases.
Prefund the dedicated buyer with at most 0.810 USDC; this includes the purchases
and is not an additional charge. Reserve at most 1.50 USDC-equivalent for the
backing provider and 0.10 USDC-equivalent for network fees. Including the prior
7.00 USDC allocation, the conservative total is 9.41 of the shared 10.00 USDC
cap. Refresh capacity and quotes before dispatch and refuse any action that
exceeds these ceilings; unknown outcomes remain reserved until reconciled.

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

## Scoped host identity evidence for acceptance

Before funding the two acceptance jobs, persist their UUIDs and independent
32-byte random challenges. Set the broker-only
`COVENANT_COMPUTE_BROKER_HOST_KEY_EVIDENCE_JOBS` to
`uuid:64lowercasehex,uuid:64lowercasehex`. At most two unique nonnil UUIDs are
accepted; invalid configuration refuses broker startup. Only these exact jobs
receive a Vast `onstart` hook, which waits at most 60 seconds for the existing
`/etc/ssh/ssh_host_ed25519_key.pub`, then emits:

```text
COVENANT_SSH_HOST_KEY_V1 <job-uuid> <challenge> ssh-ed25519 <public-key-base64>
```

The hook never generates keys, reads a private key, changes SSH settings or
emits environment variables. Default launches have no hook. Retrieve startup
logs through the authenticated provider API, validate the instance ID, job
label, machine ID and endpoint, then validate and pin the public key for strict
SSH. Log download URLs need an explicit provider-host allowlist, no redirects
and no forwarded bearer credential. A startup marker alone is not hardware
attestation or full adoption-package-16 host-identity support. Remove the
allowlist after acceptance; normal rentals remain unchanged.
