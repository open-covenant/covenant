# Conformance

Covenant freezes its public wire and record contracts as committed,
byte-exact **golden vectors**. A golden vector is the canonical serialized
form of one message or record, checked into the repository, that an external
implementation can encode and decode against without reading the Rust source.
Together the suites are the conformance contract: if your implementation
reproduces every committed vector byte-for-byte and recomputes every committed
hash, it speaks the same protocol Covenant does.

This page is the entry point. Each suite also carries a local README or module
doc with the per-contract detail.

## The suites

| Suite | Crate | Freezes | Vectors |
|---|---|---|---|
| IPC requests | `covenant-ipc` | Every daemon IPC `Request` variant, one canonical frame each | `tests/golden/requests/<kind>.json` |
| IPC responses | `covenant-ipc` | Every `Response` variant plus the protocol-info and v2 streaming envelopes | `tests/golden/responses/<kind>.json`, `tests/fixtures/v2/`, `tests/fixtures/protocol-info.v1.json` |
| Capability grammar | `covenant-permissions` | One `Capability` grant per scope namespace — the dotted action grammar | `tests/golden/capabilities/<namespace>.json` |
| Audit record kinds | `covenant-audit` | Every `AuditKind` variant's record wire form and SHA-256 event hash | `tests/fixtures/audit-kinds.v1.json` |
| Audit provenance chain | `covenant-audit` | The capability-family provenance records and the genesis-seeded chain root fold | `tests/fixtures/provenance-records.v1.json` |

The wire form is what the bytes mean, not how they are pretty-printed: the
audit suites pin the **compact** `serde_json` string because that is the exact
byte sequence hashed into the tamper-evident chain. The IPC and capability
suites pin the pretty-printed form because those vectors exist to be read and
diffed, and JSON object key order — not whitespace — is the contract.

## Running the suite

One command runs every suite:

```bash
node agent-os/scripts/conformance.mjs
```

It executes each golden runner through `cargo` and requires every suite to run
at least one test and report zero failures, so a renamed or deleted suite
surfaces as a hard error rather than a silently shrinking run.

Two read-only modes need no Rust toolchain:

```bash
node agent-os/scripts/conformance.mjs --list    # print the registered suites
node agent-os/scripts/conformance.mjs --check    # static integrity, no cargo
```

`--check` is part of `bash agent-os/scripts/validate.sh --scripts`. It discovers
the golden suites on disk — both the `tests/golden_*.rs` runners and the inline
`*_golden_vectors_are_frozen` tests in crate sources — and fails if any one is
not registered in the runner, so a new conformance suite can never be added
without the runner learning about it. It also asserts every declared fixture is
present.

## Proving conformance from another implementation

You do not need the runner — or Rust — to verify conformance. The committed
vectors are plain JSON. For a wire suite (IPC, capabilities):

1. Construct the message in your implementation.
2. Serialize it to JSON.
3. Compare against the committed vector. Object key order and field presence
   are part of the contract; insignificant whitespace is not.
4. Parse the committed vector back and confirm it round-trips to the same value.

For the audit record suites, the hash is the contract:

1. Serialize the `AuditEvent` to its compact JSON form (`canonical_json`).
2. Confirm those bytes equal the committed `canonical_json`.
3. Compute `sha256(canonical_json)` and confirm it equals `event_hash_hex`.
4. For the chain, fold the records in order: seed `previous_hash` with 64 zero
   hex chars, and for each record set
   `chain_hash = sha256(previous_hash + "\n" + event_hash)`, then carry
   `chain_hash` forward. The final value is `chain_root_hash_hex`.

See [docs/audit-integrity.md](./audit-integrity.md) for the full chain
construction and [docs/protocol-versioning.md](./protocol-versioning.md) for the
IPC versioning model.

## Changing a frozen contract

Golden vectors are never regenerated blindly to make a failing test pass — a
silent regenerate is exactly how a downstream verifier breaks. Each suite
detects drift by re-serializing the in-code value and asserting byte-for-byte
equality with the committed file, and keeps the corpus exhaustive with a
compile-time `match` over the underlying enum, so a new variant fails the build
until a vector is added.

When a wire shape genuinely changes, update the generator, re-bless, and review
the resulting diff as the record of the change:

```bash
cd agent-os
COVENANT_BLESS_IPC_GOLDEN=1        cargo test -p covenant-ipc --test golden_requests --test golden_responses
COVENANT_BLESS_CAPABILITY_GOLDEN=1 cargo test -p covenant-permissions --test golden_capabilities
COVENANT_BLESS_AUDIT_KINDS_GOLDEN=1 cargo test -p covenant-audit --test golden_audit_kinds
```

Bump the `.v<n>` suffix on a fixture file for an incompatible record shape so
existing verifiers can pin the version they support.
