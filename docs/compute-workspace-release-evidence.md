# Encrypted workspace release evidence

Status on 2026-10-18: **backend release accepted: immutable artifacts, deployed
owner-local recovery, copied-state rehearsal, cross-lease restore, settlement
and ciphertext cleanup verified. Website/card deployment remains separate.**. This is adoption package 3, independently
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
| Offline copied-state rehearsal harness | Two synthetic qualification tests passed; scoped Clippy passed; actual production-copy rehearsal passed; see deployed evidence below |

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

## Published artifacts and deployed owner-local acceptance

[PR #344](https://github.com/open-covenant/covenant/pull/344) merged at
`97550b20f256469d0b58e7b2a35e22cc0745b7fd`. The immutable
[compute-v0.3.0 release](https://github.com/open-covenant/covenant/releases/tag/compute-v0.3.0)
uses that exact source. Anonymous downloads, archive checksums, five signature
checks and four source-provenance checks passed. Four installed native commands
reported 0.3.0 and the buyer's workspace command help passed. Both public images
were downloaded anonymously in full: nine coordinator layers and six node
layers, with every layer, manifest and config hash verified against the revision.

| Component | Verified immutable digest |
| --- | --- |
| Coordinator | `sha256:d9ec5e8e12ea8582f85c2f9d11c546dfb68c692e65958d5c3d6e5f1788c7a156` |
| Node | `sha256:473a1a911cb5784936e2f57184f5f3fabb1f5892626ec7147cbd6348fb1fe8b0` |

The coordinator was deployed with writes disabled, then enabled on the same
image after R2 qualification. At 22:15 UTC on 2026-10-18, the released buyer saved
and recovered a 7,424-byte synthetic archive through the live API and R2. The
7,484-byte encrypted snapshot committed as version 1. A process interruption
preserved the pending upload and ciphertext; a later process reused them.
Same-buyer backup/import and exact-byte restore passed. Wrong-owner import and
download, stale local/remote versions, invalid leases, overwriting an existing
output and starting a second save while an upload was pending were refused.
This establishes owner-local storage recovery, not transfer to another GPU.

The actual copied production workspace journal was replayed twice and with a
torn trailing record by the workspace-only offline harness. Complete state
matched, the source copy remained unchanged, and no provider or financial worker
ran. It contained one committed snapshot and one allocated object, 7,484
reserved ciphertext bytes, no pending uploads and no garbage at capture time.
Separately, the original 19,239,073-byte financial journal prefix retained the
same SHA-256 after deployment. This proves prefix preservation; it does not
assert that no legitimate later financial records can be appended.

The initial installed-client record requested deletion while cleanup remained
pending. At 22:24:52 UTC, a later authenticated R2 HEAD covered every allocation
in that copied journal and verified the stored object had zero bytes and the
native empty SHA-256. The private empty fence remains permanently. This proves
removal of that synthetic encrypted payload. A separate authenticated production
journal read at 22:31:49 UTC, after coordinator restart, confirmed durable
deletion: one retained fence, no current object, no pending upload, no garbage
and zero reserved ciphertext bytes. The 558-byte journal copy matched the
accepted checkpoint identity and ciphertext hash. Its SHA-256 is
`98a71e375f6611eea9fc303727fb7909c6cfaa2c20837efd0f849e6eee61acbe`.
The earlier provider and installed-client reports remain unchanged; this later
record supplies the independent durable-state check.
The [sanitized acceptance progress](compute-evidence/workspaces/2026-10-18/acceptance-progress.json)
keeps these dated checks separate; public-release acceptance remains pending.

## Separate RPC incident

During rollout, the previously configured Helius account-read requests returned
HTTP 429 with `max usage reached`, although health and slot requests still
succeeded. Restart cleared the in-memory stake verdict, so the unchanged stake
gate kept operators unmatchable until a fresh chain read could succeed. The
broker's active stake was independently verified; the gate was not weakened.
The separate incident repair changes five L1 RPC settings to the official Solana
endpoint while preserving ER endpoints and financial identities. This is a
temporary availability fallback: [Solana's public RPC](https://solana.com/docs/references/clusters)
is rate limited and is not intended as permanent production infrastructure.
At 22:27:04 UTC, the same coordinator revision and image were live after that
repair. The broker was staked, live and matchable, all seven registered operators
were matchable, and the observer completed its two-lease scan with no findings.
This is a dated availability check, not a workspace storage guarantee or evidence
of a successful new paid rental. Alert-recovery delivery is not claimed here.

## Cross-lease restoration and settlement

The real acceptance used two 900-second maximum leases with the released 0.3.0
buyer. On the first authenticated L40S instance, the runner created and fetched
a synthetic 143,360-byte checkpoint archive. The client encrypted it into a
143,420-byte version-1 snapshot in private R2 storage. The first lease completed,
its signed receipt and on-chain payout were verified, and the complete provider
inventory was empty before the later lease was admitted.

The released buyer restored using the second lease's authorization. The restored
archive and its explicit SSH copy on the later instance matched the source
SHA-256 `5be69dd8664560c210e9a629b18fb73d5de501e5f8ee2be0188af78978ce50eb`.
The ciphertext SHA-256 was
`036fedf977fd837c95156449609d0a0c580b2595b5731678ad9a323b1cccb524`.
Both SSH sessions pinned host keys from authenticated provider evidence.

The instances were distinct (`56628067` then `56628918`) but used the same
physical L40S machine (`39312`). This demonstrates recovery in a later lease,
not migration between different physical hosts. The archive was transferred and
hashed, not extracted; no training run, automatic synchronization or
hardware-attested confidential execution is claimed.

| Lease | Final charge | Verification |
| --- | --- | --- |
| First | 0.118562 USDC | Signed receipt and [on-chain payout](https://explorer.solana.com/tx/4ob7prU3o9Zfy59qsT9QjssURTeJKqFfiRbqTjTNytidT61vQd8Z83tCchNcsDRXyWumqrjRKrKpJWVFZXgwoFxk) verified |
| Later | 0.049697 USDC | Signed receipt and [on-chain payout](https://explorer.solana.com/tx/34KJ13Y5VofFMAmx2rnCprkT6BDuRxnM3AZUiJy6nHnZXLjDRqiDBidpwj7XDWTtPiaptWvh3eTvqTGewxV7nM3R) verified |

The total charge was 0.168259 USDC. The final buyer ledger reconciled 0.610000
USDC deposited, 0.168259 charged, zero withdrawn and 0.441741 available. The
0.432941 unused portion of the combined lease ceilings plus the additional
0.008800 prefunding equals that available balance. This is unspent buyer credit;
no separate refund transfer is claimed. At 22:38:32 UTC, an authenticated,
complete provider inventory confirmed both instances deleted and none remaining.

The [sanitized cross-lease evidence](compute-evidence/workspaces/2026-10-18/cross-lease-acceptance.json)
records backend release acceptance with the exact scope of each check. Website
deployment is separate; the acceptance record does not claim it has occurred.

## Final checkpoint and acceptance configuration cleanup

Before deletion, a complete authenticated listing restricted to this workspace
found one object. Its native checksum and 143,420-byte size matched the saved
ciphertext. The released CLI's signed owner deletion was accepted at 22:39:43
UTC and reported cleanup scheduled after outstanding grants expired.

At 22:47:51 UTC, authenticated HEAD of that exact captured object returned 200,
zero bytes and the native empty SHA-256. The private mapping hash binds the
before/after probes without publishing an object key or signed URL. The
ciphertext is removed; a permanent private empty fence remains to prevent a
late conditional upload from recreating it.

No post-deletion journal copy was obtained for this cross-lease checkpoint.
The current cross-lease coordinator journal state is therefore not asserted.
The earlier standalone checkpoint's actual post-restart journal demonstrated
durable deletion separately. These are distinct evidence boundaries, and the
final acceptance JSON preserves them.

The temporary acceptance configuration was removed from the broker, with all
unrelated environment settings, image, service plan and disk preserved. The
same-image cleanup deployment was live at 22:48:20 UTC. Durable broker queue
counts were not reobserved after that restart; no new queue-state claim is made.
Both paid leases had already completed and their provider instances were deleted.

## Website publication

The backend and released-client acceptance is complete. Publish and verify the
release page and sharing image next, then supply the launch post and visual.
The backend evidence does not claim a website revision or sharing card has
already been deployed; those checks are recorded separately by the site release.

See [deployment and recovery steps](compute-workspace-deployment.md) and
[operation, CLI and backup instructions](compute-workspaces.md). Presigned
URLs, keys, real checkpoint contents and provider credentials must not appear in
public release evidence.
