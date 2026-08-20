# Production audit: Covenant Timeline release workflow

## Executive summary

The `v0.1.0-alpha.1` integration is ready to merge as a fixed-release shadow
audit. It persists temporal state across process boundaries, preserves a
timestamp correction, rejects conflicting writers, and verifies stored proof
receipts with the published Timeline package.

It is not a release authority or a general release service. Before applying the
workflow to another release, the fixed identifiers and expected results need
to move into a content-addressed release manifest. Promotion into the live
release path also requires authenticated GitHub evidence and operational
experience from at least one multi-day shadow run.

## P0: block release

No open issues.

## P1: required for this shadow audit

- [x] Serialize concurrent initialization and reconciliation with an operating
  system lock held across validation and atomic persistence.
- [x] Reconstruct the pre-restart event prefix from admitted evidence and
  reject coordinate or digest tampering.
- [x] Bind every observation and the readiness fact to the exact tag commit.
- [x] Distinguish the earlier readiness result for a different commit from the
  later result for the released commit.
- [x] Verify generated conclusions and checked proof receipts in a separate
  Node consumer using the exact published Timeline dependency.

## P2: required before general production use

- [ ] Replace the one-release verifier constants with a signed or
  content-addressed manifest containing the repository, tag, commit, evidence
  identities, query, and expected semantic result.
- [ ] Capture GitHub release metadata as authenticated evidence with the
  release ID, tag target, retrieval time, and issuer provenance.
- [ ] Run the workflow in shadow mode during a future multi-day release before
  attaching immutable Timeline artifacts to the release.
- [ ] Define retention, alerting, and operator ownership before making temporal
  inconsistency a release-control signal.

The fixed audit does not need these capabilities because its scope, inputs, and
expected conclusions are checked into one reviewable directory.

## P3: hardening

- [ ] Add fault-injection coverage around temporary-file sync, rename, and
  directory sync before relying on the writer as a general durability layer.
- [ ] Add structured operational events if the command moves from CI into a
  long-running service.

## Security assessment

- Rust and Node reject JSON inputs larger than 1 MiB before parsing.
- Initialization never overwrites existing state.
- Reconciliation requires the original evidence, validates the complete
  persisted prefix, and accepts one writer at a time.
- State replacement is atomic and followed by a directory sync.
- The verifier confines the readiness source to the repository and resolves
  symlinks before reading it.
- The verifier checks evidence identities, the tag commit, the installed
  package version, run identity, event bindings, semantic results, and stored
  proof receipts.
- CI installs with lifecycle scripts disabled, audits production dependencies,
  verifies registry signatures and npm provenance, and uses an exact lockfile.
- GitHub timestamps remain admitted observations rather than authenticated
  attestations. Public documentation states that boundary.

No secret material is read or emitted by this workflow.

## Performance and scale

The checked run has six events and three record cuts. Runtime and memory use are
negligible at this scale. The 1 MiB input cap prevents unbounded JSON loading.
Before generalizing to larger histories, benchmark replay cost against the
Timeline event and proof limits and define an archive strategy.

## Reliability and recovery

The initial and reconciliation commands run in separate processes against a
persisted run. A persistent sibling lock file provides crash-safe advisory
ownership; the operating system releases the lock if a process exits. Writes
use a same-directory temporary file, file sync, atomic replacement, and
directory sync.

The run can always be reconstructed from the three admitted observations. The
verification report is derived from the run and can be regenerated without
mutating it.

## Test coverage

Observed coverage includes:

- process restart and exact fixture reproduction;
- concurrent initialization with one winner;
- duplicate and conflicting reconciliation with one winner;
- busy-lock handling and lock release;
- altered coordinates and evidence digests;
- mismatched and malformed commit identities;
- oversized evidence and state inputs;
- strict UTC timestamp parsing;
- readiness-source digest and symlink escape;
- changed tagged commit;
- altered stored proof receipts;
- byte-for-byte report regeneration;
- legacy `v0alpha1` adapter compatibility.

The repository script suite, Rust formatting, Clippy with warnings denied,
adapter tests, Node tests, dependency audit, signature verification, and npm
provenance verification are release checks for this change.

## Action plan

1. Merge the fixed-release shadow audit with its checked run and receipts.
2. Use the next release to design the content-addressed manifest from real
   operational inputs.
3. Capture authenticated release evidence and publish immutable shadow
   artifacts without affecting release authority.
4. Review the shadow run before deciding whether any Timeline conclusion
   should become a release gate.
