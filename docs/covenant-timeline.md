# Covenant Timeline

Covenant uses [Covenant Timeline](https://github.com/open-covenant/covenant-timeline)
to preserve and verify temporal conclusions across process restarts. The
integration is deliberately separate from release authority: Timeline can show
what follows from admitted release records, but it cannot approve a release,
authenticate an operator, or establish that an artifact was safe.

The checked example covers Covenant `v0.1.0-alpha.1`. It starts from a
provisional publication time, persists the run, resumes in a second process,
records GitHub's authoritative publication time, and retracts the provisional
assertion. It measures publication against the readiness record for the exact
tagged commit. A separate Node verifier replays the history with the published
`@covenant-org/timeline` package and verifies a proof receipt at each record
cut.

## Release chronology

The public records contain three UTC timestamps:

| Record | Timestamp |
| --- | --- |
| GitHub release created | `2026-05-28T08:33:12Z` |
| GitHub release published | `2026-05-28T08:35:45Z` |
| Tagged-commit readiness evidence generated | `2026-05-28T08:41:45.698Z` |

The first process used the release creation time as an explicitly provisional
proxy for publication. That produced a tagged-commit readiness lag of `513698`
milliseconds. After restart, the workflow admitted GitHub's `publishedAt`
value. The intermediate record cut is inconsistent because two different
exact coordinates temporarily describe the same publication point. Retracting
the provisional assertion restores consistency and produces the authoritative
lag of `360698` milliseconds, a `153000` millisecond correction.

The run therefore preserves three distinct, verifiable answers:

| Record cut | Admitted publication evidence | Result |
| --- | --- | --- |
| Initial | Provisional `createdAt` proxy | `513698` ms |
| Correction admitted | Provisional and authoritative values | Inconsistent |
| Reconciled | Authoritative `publishedAt` value | `360698` ms |

The inconsistency is part of the evidence, not an error to hide. It makes the
correction visible without rewriting the earlier state.

The readiness file present at the release tag contained an earlier
`ready: true` result for commit
`a13a3481834a76e7868e85fac88ce2e618365a6a`, generated at
`2026-05-27T22:58:51.814Z`. The release tag points to
`94e7af53c2224aa40762c2061ac96cab34950b71`. Those records describe different
code and are not interchangeable. The checked observation and verifier bind
the later readiness result to the exact tagged commit.

## What the integration verifies

The Rust workflow:

- validates the release observations before admission;
- binds every Timeline assertion to the canonical digest of its source
  observation;
- writes the initial run atomically;
- reloads that run in a separate process and reconstructs its initial prefix
  from the original evidence before recording the correction; and
- rejects duplicate reconciliation, altered prefix state, and state that no
  longer matches the workflow contract.

The Node verifier:

- parses the persisted `v0alpha3` run with
  `@covenant-org/timeline@0.0.0-alpha.2`;
- checks the readiness observation against the committed source bytes and
  SHA-256 digest, including the tagged commit identity;
- checks each assertion and retraction against its admitted observation;
- reasons over the initial, transitional, and reconciled record cuts; and
- verifies the proof receipt returned for each conclusion.

The checked artifacts live under
[`docs/releases/v0.1.0-alpha.1/timeline`](./releases/v0.1.0-alpha.1/timeline/).

## Trust boundary

This is a shadow audit. It does not block or authorize GitHub publication. The
committed observations record GitHub fields, but the repository does not
provide a GitHub attestation for those values. The verifier proves that each
conclusion follows from the admitted records and that the readiness observation
still matches its committed source file.

The chronology shows that the release was published six minutes and 0.698
seconds before the readiness record for the tagged commit was generated. The
earlier readiness result covered a different commit. Neither record proves
that the tagged release state had passed the same checks at publication time.
The example exposes that distinction; it does not determine whether the
release was safe.

Covenant remains responsible for evidence authentication, admission policy,
capability enforcement, and release controls. Timeline remains an independent,
portable reasoning kernel.

## Versioned surfaces

`agent-os/crates/covenant-timeline-adapter` retains the original `v0alpha1`
checkpoint adapter and frozen engineering fixture for compatibility. The
release workflow uses the `v0alpha3` temporal contract. Its verifier pins the
published npm package exactly at `0.0.0-alpha.2`; it does not depend on a
workspace copy of Timeline.

The Node verifier is a separate consumer of the published package, not a second
implementation of the Timeline protocol and not an independent operator.

## Reproduce the run

Run the Rust workflow test, which starts the release command twice and compares
the reconciled state with the checked fixture:

```sh
cargo test \
  --manifest-path agent-os/Cargo.toml \
  -p covenant-timeline-adapter \
  --locked
```

Install the verifier's exact dependency set, verify the checked receipts, and
run its tamper tests:

```sh
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

To reproduce the restart boundary directly, use a new temporary state path:

```sh
state_dir="$(mktemp -d)"

cargo run \
  --manifest-path agent-os/Cargo.toml \
  -p covenant-timeline-adapter \
  --bin covenant-timeline-release -- \
  initial \
  --created docs/releases/v0.1.0-alpha.1/timeline/evidence/release-created.json \
  --readiness docs/releases/v0.1.0-alpha.1/timeline/evidence/readiness-recorded.json \
  --state "$state_dir/run.json"

cargo run \
  --manifest-path agent-os/Cargo.toml \
  -p covenant-timeline-adapter \
  --bin covenant-timeline-release -- \
  reconcile \
  --created docs/releases/v0.1.0-alpha.1/timeline/evidence/release-created.json \
  --readiness docs/releases/v0.1.0-alpha.1/timeline/evidence/readiness-recorded.json \
  --state "$state_dir/run.json" \
  --published docs/releases/v0.1.0-alpha.1/timeline/evidence/release-published.json

node agent-os/crates/covenant-timeline-adapter/verifier/verify.mjs \
  --generate \
  --run "$state_dir/run.json" \
  --evidence-dir docs/releases/v0.1.0-alpha.1/timeline/evidence \
  --repository-root . \
  --output "$state_dir/verification.json"

node agent-os/crates/covenant-timeline-adapter/verifier/verify.mjs \
  --run "$state_dir/run.json" \
  --evidence-dir docs/releases/v0.1.0-alpha.1/timeline/evidence \
  --repository-root . \
  --report "$state_dir/verification.json"

cmp "$state_dir/verification.json" \
  docs/releases/v0.1.0-alpha.1/timeline/verification.json
```

## Model-evaluation boundary

Timeline's preregistered GPT-5.6 Sol evaluation did not pass the standalone
model-memory accuracy gate. Its recorded decision was `kill`. Timeline produced
`106/108` exact answers, `106/108` exact end-to-end artifacts, `0.9574`
assertion F1, and `108/108` verified proofs. It beat bounded narrative memory
(`65/108`) but did not beat stateless full-context structured extraction
(`107/108`).

Covenant therefore does not claim that Timeline improves frontier-model answer
accuracy over a simpler structured-extraction pipeline. This integration tests
different properties: durable temporal state, explicit correction, historical
replay, and portable proof verification. The
[complete benchmark result](https://github.com/open-covenant/covenant-timeline/releases/tag/model-eval-v1-gpt-5.6-sol-2026-07-31)
is public.

The
[production audit](./production-audit-covenant-timeline-release-workflow.md)
records the fixed audit's release criteria and the requirements that remain
before it can become a reusable release service.
