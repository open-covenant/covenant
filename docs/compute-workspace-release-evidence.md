# Encrypted workspace release candidate

Status on 2026-10-18: **implemented, locally qualified and accepted by R2; not deployed or
accepted for public announcement**. This is adoption package 3, independently
scoped from framework clients in package 5.

Baseline: `deploy/compute-coordinator` at
`b2662878a4c4aaa10ee6dcd3a05a3ef1e22615de`. The release selectively adapts the
unreleased Covenant workspace modules from `codex/compute-prism-parity` rather
than merging the broader parity branch. The original upstream reference is
[Prism PR 179](https://github.com/prismnetwork-tech/prism/pull/179).

## Implemented behavior

- A buyer saves an encrypted checkpoint file independently of a GPU lease.
  HKDF-SHA-256 derives purpose-separated keys; authenticated chunks bind owner,
  workspace, version, trust floor, total length and chunk position. The master
  key and plaintext stay on the buyer.
- Signed owner requests reserve an immutable object before a short-lived direct
  upload grant is returned. The provider must enforce `If-None-Match: *` and
  verify SHA-256. Commit checks its stored checksum and size, then advances the
  version durably. Concurrent commits compare the entire reservation, including
  object key and expiry, after awaiting the provider.
- Local CLI pins, private backups and prepared ciphertext survive restart.
  Uncertain uploads use the same ciphertext, reservation and version. Restore
  requires the exact pinned snapshot and publishes a new local file atomically;
  existing destinations are never overwritten.
- Lease-scoped restore requires the same buyer's accepted, unclosed lease with
  time remaining, and the grant cannot outlive that paid window. Stronger
  hardware trust is unavailable until a separately accepted attestation service
  exists; self-reported hardware declarations do not satisfy it. Owner-local
  backup recovery is explicitly separate from release into a lease.
- Metadata uses a separate versioned, exclusively locked journal with torn-write
  recovery and atomic compaction. Existing lease, balance and settlement records
  are not rewritten. Tombstones, quota counters and cleanup obligations survive
  restart. Cleanup waits out outstanding write grants and retains reservations
  until a zero-byte fence is checksum-verified. The private empty fence remains
  permanently and blocks delayed conditional PUTs from recreating ciphertext.
- The default global ciphertext ceiling is 1 GiB, including pending objects and
  garbage. A separate 4096 lifetime object-allocation cap includes permanent
  fences and survives restart. Issuance/HEAD checks are limited to 256 per owner and 4096 globally per
  UTC day, durably across restarts and backward clock changes. These application
  limits do not constitute a provider bill cap: valid signed URLs can be reused
  until they expire.
- Workspace routes require explicit storage configuration. New admissions are
  off by default. Disabling `COVENANT_COMPUTE_WORKSPACE_WRITES_ENABLED` preserves
  existing commit recovery, owner reads and cleanup.

## Local evidence

All commands ran in the dedicated `codex/compute-workspace-release` worktree.
The following full-suite figures describe the initial implementation; the final
recovery and host-hook changes have the focused qualification recorded below.

| Check | Result |
| --- | --- |
| Complete protocol library suite | 296 passed |
| Complete coordinator library suite | 363 passed |
| Complete buyer library suite | 245 passed; one existing ignored test |
| Buyer CLI binary tests | 41 passed |
| New CLI child-process integration | Passed |
| Focused workspace coordinator tests | 11 passed, included in the coordinator total |
| Protocol workspace crypto tests | 4 passed, included in the protocol total |
| Scoped Clippy, warnings denied | Passed for affected libraries, CLI and provider acceptance example |
| Both Compute documentation validators | Passed |
| Offline copied-state rehearsal harness | Two synthetic qualification tests passed; scoped Clippy passed; production copy still pending |

The child-process integration uses the actual CLI and coordinator router with
a local conditional-write object fixture. It simulates a committed PUT whose
success reply is lost, then starts another CLI process: the retry uses the
original object, commits one version, restores identical bytes, exports a
private backup, imports it into the same buyer on a later client, and confirms
cleanup. Another buyer cannot import the private state. Plaintext does not
appear in the fixture's stored object. This is isolated integration evidence,
not Cloudflare acceptance or proof of a real GPU transfer.

Additional negative controls cover changed owner, method, signed body, snapshot
context and key; stale versions; existing output files; expired or foreign
leases; unsupported trust floors; schema mismatch; concurrent journal writers;
quota exhaustion across restarts; and a delayed HEAD racing with replacement
of an expired reservation using the same upload ID.

## Isolated Cloudflare R2 acceptance

Following the account owner's approval of R2 with application limits and their
completion of billing setup, the private Standard bucket
`covenant-compute-workspaces` was created on 2026-10-18. Public access is disabled.
An account credential is restricted to object read/write access in that bucket;
it has no bucket-administration permissions and is stored privately.

The real provider probe at 21:15:37 UTC on 2026-10-18 passed
all eight checks: initial PUT, rejection of a duplicate conditional PUT (412),
rejection of a corrupt SHA-256 (400), provider-verified HEAD checksum and size,
anonymous object-read rejection, exact authenticated GET bytes, expired-grant
rejection and deletion of both synthetic keys. It returned `accepted: true`,
exit status 0 and no pending cleanup keys. Run ID:
`a0fbdaca-9634-4abe-9eed-41fab33de505`.

The [sanitized verdict](compute-evidence/workspaces/2026-10-18/provider-acceptance.json)
has SHA-256
`1e92edfaf5b0c9631b7011c21015c44eb33be191160fcc34ed8a415633ffb52e`.

The compiled provider example used source
`be040340bb14f6152cd27df262e8dbab4b7c3917`. Only tiny synthetic test content was
written. No buyer checkpoint, paid Compute job or production configuration was
used. This is provider acceptance, not deployment, installed-client acceptance
or a real GPU restore demonstration.

## Final recovery hardening and provider fence qualification

The final review identified two crash/recovery gaps before deployment. Prepared
CLI state is now published as a complete fsynced directory by atomic rename;
committed pending state is atomically retired before recursive deletion.
Interrupted local stages are cleaned under the buyer/endpoint process lock.

Cleanup now installs an unconditional zero-byte object instead of DELETE. R2
[checks conditional writes atomically at commit](https://developers.cloudflare.com/r2/reference/consistency/),
so an already-started client `If-None-Match: *` PUT cannot recreate ciphertext
after the fence. A successful fence requires PUT 200 and HEAD proof of zero
bytes with the empty SHA-256. Unknown outcomes retain full byte reservations.
Each new object is reserved before its grant escapes; retries consume no new
allocation. The global lifetime ceiling of 4096 includes current objects,
pending uploads, garbage and permanent fences. Old journal records default the
new fence counter to zero while deriving existing allocations from their rows.

A second real R2 probe at `2026-10-18T21:53:58.172000+00:00` held a conditional PUT body in flight,
installed and verified its empty fence, then finished the PUT. R2 returned 412;
HEAD still proved zero bytes and the empty checksum. All eleven checks passed,
with no unknown PUTs or outstanding cleanup. This isolated test removed its
three synthetic keys only after all writes definitively completed; production
never removes fences. Run ID: `121434f7-fe23-446d-a53e-e38d5627e1f1`.
Probe executable SHA-256: `a3bf9ce455b3b9f14633abc014680af5426d7355af7205a0cc9a398adc15cdb6`.
The [sanitized fence verdict](compute-evidence/workspaces/2026-10-18/provider-fence-acceptance.json)
has SHA-256 `9aa8c8151f4013b441bec1a34b00dc557a122896f97f2bfc2876a2afb3032b80`.

The broker also has an opt-in host-public-key startup marker for at most two
preselected acceptance UUIDs and fresh challenges. Its default launch JSON is
unchanged. It reads an existing public Ed25519 key, never creates a key or
changes SSH; provider-log identity validation and strict pinning remain required
by the acceptance runner. This is not a hardware-attestation or generic host
identity release. See the deployment instructions for its exact configuration.

Final hardening qualification (current candidate):

| Check | Result |
| --- | --- |
| Vast adapter library, including unchanged default launch payload and opt-in hook | 55 passed |
| Broker unit and market lifecycle tests | 15 passed |
| Coordinator workspace tests, including unknown fence/replay/allocation ceiling | 12 passed |
| Buyer interrupted preparation and retired-state cleanup tests | 2 passed |
| CLI child-process save/retry/restore/backup/deletion-fence integration | 1 passed |
| Offline copied-journal rehearsal tests with additive fence counters | 2 passed |
| Both Compute documentation validators | Passed: 206 knobs; 18 tools, 6 routes, 11 protocol knobs |
| Scoped Clippy with warnings denied | Passed for changed libraries, buyer/node binaries and both workspace examples |

## Remaining public-release gates

1. Rehearse copied metadata recovery, publish immutable coordinator/client
   artifacts, verify anonymous downloads and deploy the accepted coordinator
   revision with admissions off before storage qualification.
2. Use the released CLI for a real provider save/restart/retry/restore. A
   cross-lease launch story additionally requires real authenticated SSH access,
   archive hashes on the first and later machine, lease authorization checks and
   confirmed teardown. Only the root release task can allocate the shared
   approved test budget; this implementation created no paid job or transfer.
3. Enable admissions only after acceptance, publish the page and sharing image,
   verify their live revision and X-facing metadata, then supply the launch post
   and visual. Do not announce this candidate as live before those checks pass.

See [deployment and recovery steps](compute-workspace-deployment.md) and
[operation, CLI and backup instructions](compute-workspaces.md). Presigned
URLs, keys, real checkpoint contents and provider credentials must not appear in
public release evidence.
