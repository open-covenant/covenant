# Release chronology: `v0.1.0-alpha.1`

This directory contains a replayable audit of the first Covenant release. The
records show that GitHub published the release at `2026-05-28T08:35:45Z` and
the readiness evidence for the exact tagged commit was generated at
`2026-05-28T08:41:45.698Z`, a difference of `360698` milliseconds. The tag and
that readiness record both identify
`94e7af53c2224aa40762c2061ac96cab34950b71`.

The run preserves a correction instead of replacing the earlier record. Its
first process used the GitHub release creation time,
`2026-05-28T08:33:12Z`, as an explicitly provisional publication proxy. A
second process loaded the persisted state, admitted GitHub's authoritative
`publishedAt` value, and retracted the proxy.

## Replay

The query measures:

```text
tagged-commit-readiness-recorded - artifacts-published
```

| Recorded through | State | Verified conclusion |
| --- | --- | --- |
| Sequence 3 | Creation time used as the provisional publication proxy | `513698` ms |
| Sequence 4 | Provisional and authoritative publication times both active | Inconsistent |
| Sequence 5 | Provisional publication assertion retracted | `360698` ms |

The transitional inconsistency is expected. Both active assertions bind the
same Timeline point to different exact coordinates. The retraction resolves
the conflict while leaving the earlier record cut reproducible. The correction
changes the measured lag by `153000` milliseconds.

## Contents

- `evidence/release-created.json` records the provisional GitHub `createdAt`
  observation.
- `evidence/release-published.json` records the authoritative GitHub
  `publishedAt` observation.
- `evidence/readiness-recorded.json` binds the readiness timestamp to
  `../evidence.json` by commit, path, and SHA-256 digest.
- `run.json` is the persisted and reconciled Timeline `v0alpha3` run.
- `verification.json` contains the run digest, evidence digests, conclusions,
  and verified proof receipts for all three record cuts.

The Rust producer lives in
`agent-os/crates/covenant-timeline-adapter`. The separate verifier under
`agent-os/crates/covenant-timeline-adapter/verifier` consumes the exact
published dependency `@covenant-org/timeline@0.0.0-alpha.2`.

## Verify

From the repository root:

```sh
cargo test \
  --manifest-path agent-os/Cargo.toml \
  -p covenant-timeline-adapter \
  --locked

pnpm --dir agent-os/crates/covenant-timeline-adapter/verifier \
  install --frozen-lockfile --ignore-workspace --ignore-scripts
pnpm --dir agent-os/crates/covenant-timeline-adapter/verifier \
  audit --prod --audit-level high --ignore-workspace
npm --prefix agent-os/crates/covenant-timeline-adapter/verifier \
  audit signatures
pnpm --dir agent-os/crates/covenant-timeline-adapter/verifier \
  run verify:fixture
pnpm --dir agent-os/crates/covenant-timeline-adapter/verifier test
```

Verification fails if the run no longer matches its observations, if the
readiness source bytes change, or if a proof receipt does not verify.

## Scope

This evidence is a post-release shadow audit. It did not authorize or block the
release, and it does not prove that the release was safe. The GitHub
observations are admitted records, not authenticated GitHub attestations. The
proof receipts establish what follows from those records at each cut.

The readiness file present at the tag recorded an earlier `ready: true` result
for commit `a13a3481834a76e7868e85fac88ce2e618365a6a`, generated at
`2026-05-27T22:58:51.814Z`. It covered different code. The later record cannot
establish that the tagged state had passed the same checks when the release was
published. The useful result is narrower: publication preceded the
tagged-commit readiness record, and the correction from a provisional
timestamp remains reproducibly replayable.
