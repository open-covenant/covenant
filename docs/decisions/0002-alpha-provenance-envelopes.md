# 0002: Alpha provenance envelopes

## Status

Accepted.

## Context

Covenant's autonomous development loop needs public evidence that a committed change came from a tracked task, passed named verification gates, and modified the files claimed by the task record. Existing local signing primitives are not enough: a signature can prove key possession without proving task context, validation evidence, or whether metadata leaks private machine details.

The project is not ready to claim release signing, a public key registry, or transparency-log publication. Those policies require human decisions about signing identity, key custody, and external infrastructure.

## Decision

Add a repository-native provenance envelope:

- `agent-os/scripts/provenance.mjs write` generates a JSON attestation for a Git commit and autonomy task.
- `agent-os/scripts/provenance.mjs verify` recomputes the evidence from Git object data.
- `agent-os/scripts/provenance.mjs verify-all` validates committed attestations under `docs/provenance/attestations`.
- `agent-os/scripts/validate.sh` runs provenance verification with the existing autonomy and Rust gates.
- CI uses full Git history for the Rust job so historical commit attestations can be verified.

The v1 envelope records:

- subject commit hash;
- changed file list with Git blob ids and SHA-256 digests;
- autonomy task snapshot digest from the subject commit;
- transition events for the task from the subject commit;
- validation commands and recorded pass/fail/skipped status;
- explicit limits stating that the envelope is not yet a signed release or transparency-log entry.

## Consequences

- Agent-produced commits can carry public, machine-checkable provenance without a database.
- The verifier fails on local paths, personal identifiers, private key names, or other forbidden local identity patterns.
- Historical attestations remain valid because verification reads the task record and event log from the subject commit, not from the current working tree.
- The model is intentionally unsigned in alpha. Signing and transparency publication stay blocked until the project chooses a public signing identity policy.

## Follow-up

- Add optional detached signatures once signing identity and key custody are approved.
- Add release artifact subjects after the alpha release layout is fixed.
- Publish provenance entries to a public transparency log only after the signing policy is reviewed.
