# Release Scope Manifests

A release scope manifest binds a Covenant release tag to the set of autonomy tasks integrated into that release, without exposing individual task identifiers or content.

## Why a digest, not a list

The autonomy task records under `agent-os/autonomy/tasks/` and the transition log at `agent-os/autonomy/events.jsonl` are intentionally untracked engineering-process state. They include task IDs, titles, transition notes, gate decisions, and validation context that belong to the internal-loop, not the public repo.

ADR 0006's release-evidence floor requires that the project bind a release to "every autonomy task tagged as part of the release scope". Publishing per-task signed envelopes would invert the privacy decision and leak the internal task ledger into public release assets.

The release scope manifest resolves this by committing to a content digest of the canonical sorted task set, exposing only the aggregate shape (count, state distribution) that downstream verifiers can sanity-check without seeing the contents.

## Schema

`covenant.release-scope.v1`:

```json
{
  "schema": "covenant.release-scope.v1",
  "generatedAt": "2026-05-14T00:00:00.000Z",
  "release": {
    "repository": "open-covenant/covenant",
    "releaseId": "v0.1.0",
    "tag": "v0.1.0",
    "commit": "<40-hex>",
    "previousTag": "v0.0.9"
  },
  "scope": {
    "task_count": 42,
    "task_state_distribution": {
      "integrated": 40,
      "blocked": 1,
      "ready": 1
    },
    "task_set_sha256": "<64-hex>"
  },
  "non_claims": [
    "task_set_sha256 binds the canonical concatenation of in-scope task records; it does not expose ids, titles, or transition notes",
    "task_set_sha256 does not claim that every task was reviewed by a human",
    "task_set_sha256 does not claim that every task was security-relevant or release-grade individually",
    "task_count and task_state_distribution are aggregate descriptors only"
  ]
}
```

### Required fields

| Field | Type | Notes |
|---|---|---|
| `schema` | string | Must equal `covenant.release-scope.v1` |
| `generatedAt` | ISO timestamp | When the manifest was produced |
| `release.repository` | string | `owner/repo` format |
| `release.releaseId` | string | Stable release identifier; conventionally the tag |
| `release.tag` | string | Git tag pointing at the release commit |
| `release.commit` | 40-hex | Full SHA-1 of the release commit |
| `release.previousTag` | string \| null | Previous release tag; `null` for the first release |
| `scope.task_count` | integer | Number of task records included in the digest |
| `scope.task_state_distribution` | object | Histogram of task `state` values; keys MUST match the autonomy workflow states |
| `scope.task_set_sha256` | 64-hex lowercase | Content digest, see canonicalization below |
| `non_claims` | array of strings | Explicit non-assertions; at minimum the four entries above |

### Optional fields

None at v1. Future versions MAY add fields under a new schema string; verifiers MUST reject unknown fields at the top level under `covenant.release-scope.v1`.

## Canonicalization for `task_set_sha256`

The digest binds to a deterministic concatenation of in-scope task records. Reproducibility requires every step below.

1. **Selection.** Take every task record whose transition log contains an `integrated` event with timestamp strictly greater than the `previousTag`'s tag-commit timestamp and less than or equal to the release commit timestamp. (For the first release, the lower bound is the project's first commit timestamp.) Blocked or unresolved tasks are NOT in scope; only integrated tasks contribute to the digest.
2. **Sort.** Order the selected records by `id` lexicographically (Unicode codepoint order, NFC-normalized).
3. **Canonical JSON.** For each record, produce a canonical JSON serialization: keys sorted lexicographically at every level, no whitespace, no trailing newline, UTF-8 encoding. The canonical form of `null`, `true`, `false`, numbers, and strings follows JSON.stringify with stable key ordering.
4. **Concatenate.** Concatenate the canonical JSON strings with a single `\n` (0x0A) separator. No trailing separator.
5. **Hash.** SHA-256 over the UTF-8 bytes. Lowercase 64-hex.

Two operators given the same task corpus and the same `previousTag` MUST produce the same `task_set_sha256`. Any deviation indicates a missing task, a corrupted record, or a canonicalization drift.

## Binding to other release artifacts

A signed release scope manifest will be one of three artifacts in the release-evidence floor under ADR 0006:

1. `covenant.provenance.release.v1` release subject manifest — names the release artifacts and their SHA-256 digests. Signed today by [.github/workflows/sign-release-artifacts.yml](../../.github/workflows/sign-release-artifacts.yml).
2. `covenant.release-scope.v1` release scope manifest — this document. Signed by the same workflow once the generator script lands.
3. `covenant.audit-root-attestation.v1` audit-root attestation — binds to the release subject digest AND the release scope digest. Signing workflow still pending.

The audit-root attestation will embed both `releaseSubjectSha256` (already supported by `agent-os/scripts/provenance.mjs`) and a future `releaseScopeSha256` field, so a single signature over the audit-root proves both the artifact set and the task set were in scope at release time.

## Publication

Generated manifests are committed to `docs/provenance/release-scopes/<tag>.json` before the release tag is pushed, then signed in CI on release publication. The publication location is publicly tracked so verifiers can pull the manifest and run cosign verify-blob without needing internal-loop access.

`agent-os/scripts/build-release-scope-manifest.mjs` is the generator. It reads every task record under `--task-dir`, validates that each has a known `id` and an allowed `state`, sorts records by `id`, canonicalizes each record (sorted keys at every level, no whitespace, no trailing newline), concatenates with a single `\n`, and emits the full `covenant.release-scope.v1` manifest with the SHA-256 over the concatenation as `task_set_sha256`. It requires `--repository`, `--release-id`, `--tag`, `--commit`, and either `--previous-tag` or `--first-release`. With `--output <path>` it writes the manifest; otherwise it writes to stdout. The generator does not sign, publish, upload, or write transparency-log entries; signing is the CI workflow's job.

Until the signing-workflow extension lands, `docs/provenance/release-scopes/` stays empty (only its `README.md` is committed) and no release-scope claims are made.

## Verifier contract

Future verifiers will need to:

1. Pull the signed manifest, `.sig`, and `.pem` from the release.
2. Run `cosign verify-blob` with the project identity pins from [docs/provenance/keys/README.md](./keys/README.md).
3. Confirm `release.tag` matches the verifying release, `release.commit` matches the tag's commit, and `release.previousTag` matches the prior published release.
4. Confirm `task_count` equals the number of summands implied by `task_state_distribution`.
5. Optionally regenerate `task_set_sha256` locally if the verifier has access to the canonical task corpus (only the project operator has this access today).

Verifiers without access to the task corpus can still verify the signature, the schema, and the internal consistency of the count/distribution fields. They cannot independently confirm `task_set_sha256` without the corpus — that's an intentional consequence of the privacy-preserving design.

## Read-only inspection

`agent-os/scripts/release-scope-manifest.mjs --path <manifest>` performs steps 3 (release shape), 4 (count vs distribution sum), and the schema/non-claims checks from the verifier contract without recomputing `task_set_sha256`. It emits a `covenant.release-scope-manifest-check.v1` report under `--json` and exits non-zero on any failure. The inspector does not sign, publish, upload, or recompute the task-set digest, and it redacts absolute paths in its report. `agent-os/scripts/validate-release-scope-manifest.mjs` exercises a well-formed fixture plus tampered cases (bad schema, non-ISO `generatedAt`, non-hex commit, non-hex digest, distribution-sum mismatch, missing required `non_claims` entry, unknown top-level field, unknown workflow state, invalid repository slug, empty `previousTag`) so regressions in the inspector are caught before a manifest ever ships.

`agent-os/scripts/release-scope-readiness.mjs` emits a `covenant.release-scope-readiness.v1` report that tracks which release-scope publication pieces are present (schema doc, publication location, inspector) and which remain planned (generator script, CI signing workflow extension, audit-root binding to `releaseScopeSha256`). It accepts `--json` and `--strict-public`; the latter exits non-zero while public release-scope publication is blocked. `agent-os/scripts/validate-release-scope-readiness.mjs` pins the gate identifiers, the implemented-vs-planned distribution against the current repo state, and the exit-code contract for `--strict-public`, `--help`, and unknown flags.

## Non-goals

- This manifest does not publish task records.
- This manifest does not assert human review of any individual task.
- This manifest does not assert that the release passed any specific test or gate beyond what the autonomy workflow already records internally.
- This manifest does not replace the audit-root attestation; it is a separate, complementary digest.
