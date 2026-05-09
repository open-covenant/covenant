# 0002: Initial Provenance Envelopes

## Status

Accepted.

## Context

Covenant's autonomous development loop needs public evidence that a committed change came from a tracked task, passed named verification gates, and modified the files claimed by the task record. Existing local signing primitives are not enough: a signature can prove key possession without proving task context, validation evidence, or whether metadata leaks private machine details.

Release signing, a public key registry, and transparency publication are governed by separate signing identity, key custody, and external infrastructure policies.

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
- explicit release-evidence scope for the envelope.

## Consequences

- Agent-produced commits can carry public, machine-checkable provenance without a database.
- The verifier fails on local paths, personal identifiers, private key names, or other forbidden local identity patterns.
- Historical attestations remain valid because verification reads the task record and event log from the subject commit, not from the current working tree.
- The model starts unsigned. Signing and transparency publication are separate release-hardening tracks.

## Follow-up

- Add optional detached signatures once signing identity and key custody are approved.
- Add release artifact subjects after the release layout is fixed.
- Publish provenance entries to a public transparency log only after the signing policy is reviewed.
