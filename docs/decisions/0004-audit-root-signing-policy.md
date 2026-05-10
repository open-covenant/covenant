# 0004: Audit Root Signing Policy

## Status

Accepted for implementation planning.

## Context

Covenant now maintains a local audit hash-chain sidecar and exposes integrity reports through CLI, IPC, and HTTP. That gives local tamper evidence over retained audit rows, but it is not public non-repudiation. A host-level attacker can still delete or replace both the audit log and the sidecar, and there is no public evidence that a particular root existed at a particular time.

The next hardening step is to define how audit roots become signed release evidence without overstating what is implemented today.

## Decision

Use detached root attestations as the first public audit-root signing format.

Each attestation should bind:

- `schema_version`
- repository slug
- subject commit
- task id or release id
- retained audit event count
- retained sidecar anchor count
- `root_hash_hex`
- previous published root hash when available
- creation timestamp
- signing key id
- validation command evidence

Sign the canonical attestation payload with a project-controlled signing key. Store the signed attestation in the repository for releases and publish the same payload to a transparency log when the release process matures.

## Signing Identity

The signing identity must be a project identity, not an individual workstation identity. The first acceptable implementation is one of:

- GitHub Actions OIDC plus Sigstore keyless signing for release artifacts.
- A dedicated offline project key with documented custody and rotation policy.

Personal emails, local usernames, hostnames, private SSH key names, and absolute home paths must never appear in signed payloads.

## Non-Goals

- Do not sign every local audit event.
- Do not claim immutable retention from repository-stored attestations.
- Do not require a network transparency log for local development.
- Do not anchor audit roots on-chain until settlement policy and costs are defined.

## Implementation Path

1. Define an `audit-root-attestation.v1` JSON schema.
2. Add a generator that reads `covenant audit verify` output and emits an unsigned payload.
3. Add signing through the selected project identity.
4. Extend provenance verification to validate the payload, subject commit, task/release id, and signature.
5. Publish signed root attestations for release candidates.
6. Add transparency-log publication after signing is stable.

Current implementation status:

- Steps 1 and 2 are implemented in `agent-os/scripts/provenance.mjs`.
- The verifier validates unsigned `covenant.audit-root-attestation.v1` payloads, canonical commits, valid audit reports, and task snapshot bindings.
- Release-target audit-root attestations can bind an embedded `covenant.provenance.release.v1` release subject digest and reject mismatched release metadata.
- Detached ed25519 signature generation and verification are implemented with embedded public-key material. A reviewed project key custody and release publication process is still required before these signatures should be treated as public non-repudiation.

## Consequences

- Local audit integrity remains useful without network dependencies.
- Public release claims gain a concrete verification target.
- Root signing is separated from retention policy and from on-chain settlement, avoiding premature coupling.
- Until steps 1-4 are implemented, docs must continue to describe audit roots as local integrity evidence only.
