# Encrypted compute workspaces

Workspaces keep a buyer's encrypted files independently of a rented machine.
The coordinator stores versioned metadata and issues short-lived object-store
URLs. The buyer encrypts and decrypts locally with a workspace key derived from
a `VaultKey` using HKDF-SHA-256. Neither the coordinator nor the object store
receives that key.

This implementation accepts a file, such as a tar archive of a checkpoint
directory. Snapshots are limited to **4 GiB plaintext**, streamed in 1 MiB
authenticated chunks. This keeps the complete encrypted object below S3's
single-PUT limit. Multipart snapshots and automatic remote directory transfer
are not implemented. The configured global storage ceiling can be lower than
the format limit; the default 1 GiB ceiling includes ciphertext overhead.

## Storage configuration

With `COVENANT_COMPUTE_WORKSPACE_S3_ENDPOINT` absent, the service registers no
workspace routes. Enabling it requires all of:

| Variable | Meaning |
| --- | --- |
| `COVENANT_COMPUTE_WORKSPACE_S3_ENDPOINT` | HTTPS S3-compatible origin, without a path; HTTP is allowed only on loopback for testing. |
| `COVENANT_COMPUTE_WORKSPACE_S3_REGION` | Region used for SigV4 signing. |
| `COVENANT_COMPUTE_WORKSPACE_S3_BUCKET` | Private bucket. |
| `COVENANT_COMPUTE_WORKSPACE_S3_ACCESS_KEY` | Credential limited to the dedicated workspace bucket, and to its prefix if the provider supports that restriction. |
| `COVENANT_COMPUTE_WORKSPACE_S3_SECRET_KEY` | Corresponding secret, supplied through the deployment's secret store. |
| `COVENANT_COMPUTE_WORKSPACE_S3_PREFIX` | Optional prefix; defaults to `covenant-workspaces`. |
| `COVENANT_COMPUTE_WORKSPACE_S3_SESSION_TOKEN` | Optional session credential. |
| `COVENANT_COMPUTE_WORKSPACE_WRITES_ENABLED` | Defaults to `false`; set `true` only after storage acceptance. Disabling new admissions preserves commit recovery, reads and cleanup. |
| `COVENANT_COMPUTE_WORKSPACE_MAX_BYTES` | Positive global ciphertext ceiling, including current objects, pending reservations and garbage until a verified empty fence replaces the ciphertext. Defaults to 1 GiB. |
| `COVENANT_COMPUTE_WORKSPACE_MAX_OWNERS` | Positive metadata-owner limit; defaults to 1024. |

The bucket must enforce conditional `PUT` with `If-None-Match: *`, validate
`x-amz-checksum-sha256`, and return that checksum on `HEAD` with
`x-amz-checksum-mode: ENABLED`. Unsupported implementations fail commit; an
ETag or caller-written metadata value is not accepted as a checksum. Grant
signatures bind the immutable-write condition, object length and checksum.
URLs expire after five minutes. Pending uploads expire after fifteen minutes.

Use an unversioned bucket: retirement overwrites ciphertext with a permanent
zero-byte fence, and versioning would retain the previous encrypted payload.
Do not configure lifecycle rules that delete these fences. Block public access and
deny independent writes to the prefix. Enforce provider storage quotas as a
separate backstop; one owner can hold sixteen active workspaces. Deleted IDs
remain tombstoned, with a separate ceiling of 1024 total IDs per owner.
A deployment permits at most 4096 lifetime object allocations, counting current
objects, pending uploads, garbage and permanent fences. Retries reuse their
allocation. At this cap, new uploads fail closed; restart does not reset it.

Issuance of upload/download grants and commit HEAD checks is durably limited to
256 per owner and 4096 globally per UTC day; moving the clock backward does not
reset the ceiling. These are application limits, **not a provider bill cap**:
a client can reuse a valid signed URL until its expiration. Monitor the provider's
request usage and billing separately. No operator rental or payment is created
by saving a snapshot.

Durable metadata resides in `workspaces.jsonl` beside the coordinator journal.
An exclusive lock prevents concurrent coordinator writers. Its versioned journal is additive: existing job, payment and lease journals are unchanged. A failed commit never advances the visible version. Cleanup runs each minute
and retires abandoned uploads and superseded snapshots after outstanding grants
expire. It replaces ciphertext with a verified empty object that remains
permanently: the existence check on every conditional client PUT prevents an
in-flight upload from recreating deleted ciphertext. Unknown fence outcomes
retain the full byte reservation. Cleanup obligations and fence counters survive
a coordinator restart. The metadata
and object store both need backups; a lost buyer key cannot be recovered.

## Client and HTTP interfaces

`covenant_compute_buyer::workspace::WorkspaceClient` offers `create`, `list`,
`get`, `prepare_snapshot`, `upload_snapshot`, `snapshot_file` and `restore_file`.
Persist a `PreparedWorkspaceSnapshot` before upload and reload it after a crash to retry uncertain uploads or commits with the same ciphertext and reservation ID. The CLI does this automatically and pins returned `WorkspaceView` values locally, so rollback checks survive buyer restart. Restore compares
the server's response to the caller's exact pinned snapshot, authenticates the
owner, workspace ID, version and trust floor, then atomically creates a new file.
It never overwrites an existing destination.

The HTTP routes are:

| Method and route | Action |
| --- | --- |
| `POST /workspaces/{owner}` | Create, with caller-generated UUID, name and immutable trust floor. |
| `GET /workspaces/{owner}` | List the owner's active metadata. |
| `GET /workspaces/{owner}/{id}` | Read workspace metadata. |
| `DELETE /workspaces/{owner}/{id}` | Tombstone a workspace and schedule object cleanup. |
| `POST /workspaces/{owner}/{id}/upload` | Compare the expected version and reserve an immutable object. |
| `POST /workspaces/{owner}/{id}/commit` | Verify the stored size/checksum and atomically advance the version. |
| `POST /workspaces/{owner}/{id}/download` | Grant access to the exact latest snapshot, optionally for a live lease. |

Each request needs `x-compute-workspace-signed-at-ms` and
`x-compute-workspace-signature`. The owner's Ed25519 signature binds the HTTP
method, canonical route, exact request-body SHA-256 and timestamp under the
workspace-specific signature domain. Requests older than two minutes are
refused. Uploads and commits have durable IDs in addition to timestamp checks.

A download without `lease_job_id` is an owner-local backup read. A download
with that ID additionally requires an accepted, unexpired lease belonging to
the owner, with an unexhausted paid window and no pending close. Its grant cannot outlive that paid window; an already issued ciphertext URL remains usable until its short expiration. Current leases
earn only `open`: a node's `tee_capable` claim never satisfies a stronger floor.
Workspaces can be stored with stronger floors and recovered locally, but cannot
be released to current lease hardware. The buyer remains responsible for
transferring restored plaintext through its authenticated lease channel.

## CLI workflow

Build the pinned release's `covenant-compute` buyer binary. The CLI uses the
existing buyer identity under `COVENANT_COMPUTE_MCP_HOME`; it does not open a
paid lease or transfer funds. Set `COVENANT_COMPUTE_COORDINATOR_URL` to the
accepted deployment. Enable storage only after the acceptance steps below.

```sh
covenant-compute workspace create training-checkpoint --id <workspace-uuid>
covenant-compute workspace save <workspace-uuid> checkpoint.tar
covenant-compute workspace status <workspace-uuid>
# After a crash or uncertain upload/commit, use exactly the saved operation:
covenant-compute workspace retry <workspace-uuid>
# Restore locally; --lease also authorizes that buyer's live lease and trust floor.
covenant-compute workspace restore <workspace-uuid> restored.tar --lease <later-lease-uuid>
covenant-compute workspace backup <workspace-uuid> private-workspace-backup.json
covenant-compute workspace delete <workspace-uuid>
```

A checkpoint is a file; archive a directory before saving. Copy a checkpoint
from a lease and a restored archive to a later lease only over an independently
authenticated SSH channel. Neither directory synchronization nor automatic
remote transfer is claimed by this release. The `--lease` argument checks lease
authorization but does not upload plaintext to the machine.

Local pins and keys are scoped by coordinator endpoint and buyer identity,
written atomically with private file permissions and protected by a process
lock. Prepared ciphertext and its manifest are fsynced in a private staging
directory, then published together by atomic rename before dispatch. Completed
pending state is atomically retired before recursive removal. Save refuses if an earlier operation is uncertain. Restore never silently
adopts a different server version or overwrites an existing destination. A
private backup contains the encryption key and exact version pin; store it
privately. On another client with the same buyer identity and endpoint, use
`workspace import <private-backup.json>`. An older backup cannot replace a newer
local pin. Deleting a workspace preserves its tombstone and schedules stored
ciphertext for deletion after outstanding grants expire.

## Provider acceptance and promotion

On 2026-10-18, after the account owner approved R2 with application limits and
completed billing setup, the private Standard bucket
`covenant-compute-workspaces` was created. Its object credential is restricted
to that bucket and stored privately. Actual provider acceptance passed all
checks below, with the isolated synthetic keys deleted after their writes
definitively completed. This qualifies the storage
adapter; it does not establish a deployed workspace API or real cross-lease
restoration. The [evidence record](compute-workspace-release-evidence.md)
separates those remaining gates. An account's free allowance is not an unlimited
spending authorization.

Run the opt-in `workspace-storage-acceptance` example with the configured S3
environment and `--allow-synthetic-storage-writes`. It writes three tiny
synthetic objects and emits a JSON verdict after cleanup. It holds a conditional
PUT body in flight, installs and verifies an empty fence, then completes the PUT
and requires 412 with no ciphertext resurrection. An unknown PUT outcome retains
cleanup keys: expiry alone never establishes a safe deletion time. Reconcile
with a verified permanent fence or a definitively completed write. Required evidence includes
conditional-write rejection, checksum rejection, verified HEAD checksum,
private object reads, exact download bytes, expiration and successful cleanup.
No URL or provider credential appears in its report.

Then deploy the immutable coordinator with workspace writes disabled, rehearse
copied metadata recovery, and enable writes only after the provider checks pass.
Use the released CLI for a real encrypted save, process restart, original-operation
retry and restore. The public cross-lease demonstration additionally needs real
lease access and independently authenticated SSH. Retain machine, archive and
ciphertext digests as distinct evidence. Keep stronger trust classes unavailable
until the attestation release has real hardware acceptance. Roll back admissions
with `COVENANT_COMPUTE_WORKSPACE_WRITES_ENABLED=false`, leaving the storage
configuration in place so recovery and deletion continue.

## Validation status

Local tests cover chunk encryption and context substitution, signed request
authorization, version races and rollback, restart/torn-write recovery, expiry
and orphan cleanup, lease ownership/trust gates, SigV4 golden signing, and the
real client/router against a local S3-compatible conditional-write fixture.
These are local integration results. Production acceptance additionally needs
the configured provider's conditional-write/checksum tests and a real storage
round trip; no bucket is provisioned or activated by these tests.
