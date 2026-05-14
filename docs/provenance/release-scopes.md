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
2. **Sort.** Apply Unicode NFC normalization to each record's `id`, then order the records by NFC-normalized `id` using Unicode codepoint comparison (not locale-dependent collation). The same comparator applies wherever this document says "sort" or "lexicographic".
3. **Canonical JSON.** For each record, produce a canonical JSON serialization with the following rules:
    - **Encoding.** UTF-8 with no byte-order mark, no whitespace between tokens, no trailing newline. The byte sequence MUST be exactly the bytes that contribute to the digest.
    - **Object keys.** Sorted with the same Unicode codepoint comparator as step 2 (NFC-normalized keys, then codepoint comparison) at every nesting level.
    - **`null`, `true`, `false`.** The literal tokens `null`, `true`, `false`.
    - **Integers.** Decimal digits with no decimal point, no leading zeros (except the single digit `0`), and a leading `-` for negatives (never `+`).
    - **Non-integer numbers.** Use the shortest decimal representation that round-trips through IEEE 754 double precision. No trailing zeros after the decimal point. Use exponential form `1e21` (lowercase `e`, no `+`) only when the magnitude is >= 1e21 or < 1e-6, matching ECMA-262 `Number.prototype.toString`. `NaN` and `Infinity` are not valid JSON values and MUST cause the canonicalizer to error.
    - **Strings.** Standard JSON escaping: `\"`, `\\`, `\/` only when necessary, `\b`, `\f`, `\n`, `\r`, `\t` for the named controls, and `\uXXXX` (lowercase hex) for any other U+0000–U+001F codepoint. All other characters MUST appear as their literal UTF-8 bytes (no `\uXXXX` escaping for non-ASCII codepoints). High surrogates (U+D800–U+DBFF) MUST be paired with their following low surrogate; unpaired surrogates MUST cause the canonicalizer to error.
    - **Empty containers.** Encode an empty object as `{}` and an empty array as `[]` with no internal whitespace.
    - **Arrays.** Preserve the input element order. Separator between elements is a single comma `,` with no whitespace.
    - **Objects.** After key sorting, each key-value pair appears as `<canonical-string>:<canonical-value>` with no whitespace; pairs are separated by a single comma `,` with no whitespace.
4. **Concatenate.** Concatenate the canonical JSON strings with a single `\n` (0x0A) separator. No trailing separator after the last record.
5. **Hash.** SHA-256 over the UTF-8 bytes of the concatenation. Express the digest as 64 lowercase hexadecimal characters.

Two operators given the same task corpus and the same `previousTag` MUST produce the same `task_set_sha256`. Any deviation indicates a missing task, a corrupted record, or a canonicalization drift.

## Binding to other release artifacts

A signed release scope manifest will be one of three artifacts in the release-evidence floor under ADR 0006:

1. `covenant.provenance.release.v1` release subject manifest — names the release artifacts and their SHA-256 digests. Signed today by [.github/workflows/sign-release-artifacts.yml](../../.github/workflows/sign-release-artifacts.yml).
2. `covenant.release-scope.v1` release scope manifest — this document. Generated by engineering-loop tooling outside the public repository; signing by the release-artifacts workflow is still pending.
3. `covenant.audit-root-attestation.v1` audit-root attestation — binds to the release subject digest AND the release scope digest. Signing workflow still pending.

`agent-os/scripts/provenance.mjs` embeds both `releaseSubjectSha256` and `releaseScopeSha256` on the audit-root attestation when the corresponding manifests are passed at write time, and `verifyAuditRoot` re-validates each embedded scope and refuses tampered payloads. A single signature over the audit-root therefore proves both the artifact set and the task set were in scope at release time.

## Publication

Generated manifests will be committed to `docs/provenance/release-scopes/<tag>.json` before the release tag is pushed, then signed in CI on release publication. The publication location is publicly tracked so verifiers can pull the manifest and run cosign verify-blob without needing internal-loop access.

Until the release-artifacts signing workflow is extended to sign `<tag>.json`, `docs/provenance/release-scopes/` stays empty (only its `README.md` is committed) and no release-scope claims are made.

## Verifier contract

Future verifiers will need to:

1. Pull the signed manifest, `.sig`, and `.pem` from the release.
2. Run `cosign verify-blob` with the project identity pins from [docs/provenance/keys/README.md](./keys/README.md).
3. Confirm `release.tag` matches the verifying release, `release.commit` matches the tag's commit, and `release.previousTag` matches the prior published release.
4. Confirm `task_count` equals the number of summands implied by `task_state_distribution`.
5. Optionally regenerate `task_set_sha256` locally if the verifier has access to the canonical task corpus (only the project operator has this access today).

Verifiers without access to the task corpus can still verify the signature, the schema, and the internal consistency of the count/distribution fields. They cannot independently confirm `task_set_sha256` without the corpus — that's an intentional consequence of the privacy-preserving design.

## Non-goals

- This manifest does not publish task records.
- This manifest does not assert human review of any individual task.
- This manifest does not assert that the release passed any specific test or gate beyond what the autonomy workflow already records internally.
- This manifest does not replace the audit-root attestation; it is a separate, complementary digest.
