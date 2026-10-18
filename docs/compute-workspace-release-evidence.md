# Encrypted workspace release candidate

Status on 2026-10-18: **implemented and locally qualified; not deployed or
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
  until provider deletion succeeds.
- The default global ciphertext ceiling is 1 GiB, including pending objects and
  garbage. Issuance/HEAD checks are limited to 256 per owner and 4096 globally per
  UTC day, durably across restarts and backward clock changes. These application
  limits do not constitute a provider bill cap: valid signed URLs can be reused
  until they expire.
- Workspace routes require explicit storage configuration. New admissions are
  off by default. Disabling `COVENANT_COMPUTE_WORKSPACE_WRITES_ENABLED` preserves
  existing commit recovery, owner reads and cleanup.

## Local evidence

All commands ran in the dedicated `codex/compute-workspace-release` worktree.

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

## Remaining public-release gates

1. Finish enrollment of Covenant's Cloudflare account in R2. The account owner
   approved R2 with application limits; checkout still requires their payment
   method and billing address. The saved token cannot list R2 buckets. The
   intended isolated private bucket is `covenant-compute-workspaces`, Standard
   class, with scoped object credentials. No bucket or credential exists yet.
2. Run the opt-in `workspace-storage-acceptance` example against that provider.
   Require immutable-write, checksum, private-read, exact-byte, expiration and
   deletion verdicts. Unknown write outcomes retain explicit cleanup keys until
   their grants expire; they do not count as successful cleanup.
3. Rehearse copied metadata recovery, publish immutable coordinator/client
   artifacts, verify anonymous downloads and deploy the accepted coordinator
   revision with admissions off before storage qualification.
4. Use the released CLI for a real provider save/restart/retry/restore. A
   cross-lease launch story additionally requires real authenticated SSH access,
   archive hashes on the first and later machine, lease authorization checks and
   confirmed teardown. Only the root release task can allocate the shared
   approved test budget; this implementation created no paid job or transfer.
5. Enable admissions only after acceptance, publish the page and sharing image,
   verify their live revision and X-facing metadata, then supply the launch post
   and visual. Do not announce this candidate as live before those checks pass.

See [deployment and recovery steps](compute-workspace-deployment.md) and
[operation, CLI and backup instructions](compute-workspaces.md). Presigned
URLs, keys, real checkpoint contents and provider credentials must not appear in
public release evidence.
