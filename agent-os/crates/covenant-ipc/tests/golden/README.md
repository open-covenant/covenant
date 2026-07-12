# IPC Conformance Golden Vectors

This directory holds the frozen wire-shape contract for the daemon IPC
protocol. Each file is the canonical serialized form of one protocol message;
together they are the first conformance contract that external clients (the
`covenant` CLI, the HTTP gateway, third-party SDKs) can encode and decode
against without reading the Rust source.

## Layout

```
golden/
  requests/        one <kind>.json per Request enum variant
  responses/       one <kind>.json per Response enum variant (v1 terminal frames)
```

The file name is the message's wire `kind` discriminator, so
`requests/submit_intent.json` is exactly the frame a client sends for
`{"kind":"submit_intent",...}`. The `rename_all = "snake_case"` derive splits
the `A2A` token, so the A2A verbs land as `send_a2_a_task.json`,
`a2_a_queue.json`, and so on — those are the real wire slugs, not typos.

Requests share a single additive schema across protocol versions (new optional
fields are introduced with `#[serde(default)]`), so they are not version-split.
`responses/` freezes the v1 terminal `Response` frames the daemon emits by
default; list-bearing variants pin the empty-list envelope, since element
payloads keep their own crate-level shape pins. The v2 streaming envelopes
(`StreamEnvelope`) that only exist in protocol v2 are pinned separately under
`../fixtures/v2/`.

## The contract

`tests/golden_requests.rs` and `tests/golden_responses.rs` are the drift
runners; both drive the shared harness in `tests/common/mod.rs`. For every
`Request` / `Response` variant the harness:

1. re-serializes the in-code value with `serde_json::to_string_pretty` and
   asserts the result is **byte-for-byte** equal to the committed file;
2. deserializes the committed file back and asserts it round-trips to the same
   value — catching a type whose canonical form does not round-trip (e.g. a
   `serialize_with`/`deserialize_with` pair that are not inverses, or a
   `skip_deserializing` field that still serializes) that byte-equality alone
   would miss;
3. checks the corpus is **exhaustive** — every variant has exactly one file,
   with no orphans and none missing.

A compile-time `match` over each enum (`assert_request_variant_coverage` /
`assert_response_variant_coverage`) breaks the build when a variant is added or
removed, so the corpus cannot silently fall out of sync with the type.

Any unreviewed change to a message's wire shape — a renamed field, a reordered
struct, a changed discriminator, a removed field — fails the build instead of
silently breaking downstream consumers.

## Scope

Each vector pins one **fully-populated envelope** per variant — the outer
message shape. Two things are deliberately pinned elsewhere, not here:

- **`skip_serializing_if` key-absent forms.** The generators set every optional
  field, so the corpus freezes the key-present shape; the default v1 frame that
  omits e.g. `prefer_stream` is pinned by the inline serde tests in `src/lib.rs`.
- **Nested-enum alternatives.** A vector pins one case of a nested enum (e.g.
  `peer_revoked` carries `RevokeOutcome::NotFound`); the other cases an SDK must
  still decode (`revoked`, `ambiguous`, …) are pinned by their owning crate's
  serde tests. The golden corpus freezes the message envelope, not every nested
  payload.

## Regenerating (blessing)

Golden vectors are never regenerated blindly. When you intentionally change a
message shape, update the generator in `tests/golden_requests.rs` or
`tests/golden_responses.rs`, then re-bless and **review the resulting diff**
before committing:

```sh
cd agent-os
COVENANT_BLESS_IPC_GOLDEN=1 cargo test -p covenant-ipc \
  --test golden_requests --test golden_responses \
  golden_vectors_match_committed_corpus
```

The blessed files show up in `git diff` as the reviewable record of the wire
change. Adding a new variant requires three coordinated edits, each enforced by
a failing build or test: add a `match` arm, add a generator vector, and add the
slug to `EXPECTED_REQUEST_KINDS` / `EXPECTED_RESPONSE_KINDS`. Removing a variant
is the reverse plus deleting its `<kind>.json` — the exhaustiveness test flags
the orphaned file until you do.

Keep the `golden_vectors_match_committed_corpus` filter on the bless command:
it selects only the corpus writers. An unfiltered bless run mixes the writer and
the directory-reading exhaustiveness test in one binary (the latter no-ops under
bless precisely to avoid that race).
