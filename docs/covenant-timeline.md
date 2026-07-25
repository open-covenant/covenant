# Covenant Timeline Integration

Status: M3 reference adapter implemented; pre-alpha package publication pending

Covenant Timeline is an incubating standalone protocol for replaying evidenced
checkpoints in long-running software and agent work. Covenant is its first
reference adopter.

## Ownership boundary

The standalone Timeline project owns:

- contract, event, evidence, decision, command, and receipt schemas;
- deterministic reduction and replay;
- conformance fixtures;
- portable SDK behavior.

Covenant owns only the adapter between those objects and its implemented
runtime surfaces.

The Covenant repository does not copy or fork the Timeline reducer. The Rust
adapter targets standalone Timeline commit
`88da86d3dce4be33320f93db0ba4f4fc7c0643cf`, pinned in
`covenant-timeline-adapter/Cargo.toml`. The pin owns the v0alpha1 contract; the
adapter owns only Covenant-to-Timeline translation.

## Mapping

| Covenant surface | Timeline object |
| --- | --- |
| Commit-scoped provenance envelope | Evidence |
| Audit event or integrity report | Event or evidence |
| Policy and review outcome | Evidence |
| Timeline checkpoint evaluation | Decision |
| Capability request | Command |
| Runtime or settlement result | Receipt |

## Implemented adapter

`agent-os/crates/covenant-timeline-adapter` provides:

- RFC 8785/SHA-256 evidence identities over Covenant provenance envelopes,
  audit events, and audit-integrity reports;
- payload-free evidence events so private source records do not enter exports;
- checkpoint evaluation events with explicit policy and evidence references;
- translation from `covenant.capability.request` commands to the typed
  `covenant_ipc::Request::GrantCapability` wire object;
- receipt events derived from typed Covenant responses;
- a deterministic engineering-run exporter and frozen offline fixture.

Command translation rejects a schema/kind mismatch, any replay policy other
than `forbid`, a payload-template mismatch, and a capability response for the
wrong action. Translation returns a request value; it never sends the request.
Covenantd remains the authorization and execution boundary.

## Reference run

The frozen run at
`agent-os/crates/covenant-timeline-adapter/tests/fixtures/covenant-engineering-run.json`
crosses four checkpoints:

1. the M0 integration boundary was recorded;
2. the implementation resumed from PR #119;
3. adapter and standalone verifier checks passed;
4. the `@covenant-org/timeline` alpha package was packable.

The final checkpoint emits a `release.publish` capability request. The fixture
then records the typed `CapabilityGranted` conformance response as a receipt.
That response proves the adapter join, not that a production daemon granted
release authority or that the package was published.

Regenerate and compare the export:

```sh
cd agent-os
cargo test -p covenant-timeline-adapter
cargo run -p covenant-timeline-adapter --bin covenant-timeline-export-demo
```

The run verifies with Covenant stopped by passing the JSON fixture to the
standalone Timeline verifier. The current verified state digest is
`sha256:6d6d33d640e4c676bc2e8104f7c528b64830b42549ff988fe798a67cea017813`.

## Safety boundary

- Timeline decisions do not grant Covenant capabilities.
- Covenant re-evaluates authorization, expiry, scope, and operator policy.
- Replay never invokes the Covenant adapter.
- Missing or unverified provenance remains visible.
- Exported records contain no secrets or private evidence payloads by default.

The remaining release step is publishing or landing the standalone alpha
package and updating the adapter pin to that release. The fixture deliberately
records `"published": false`; this repository does not claim that npm
publication already happened.
