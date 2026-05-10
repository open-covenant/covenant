# Release Provenance Readiness

Local provenance envelopes can verify task and audit-root evidence. Public release provenance needs additional gates for artifact subjects, project key custody, and transparency publication.

Run the read-only readiness report from the repository root:

```bash
node agent-os/scripts/release-provenance-readiness.mjs --json
```

Validate the report contract:

```bash
node agent-os/scripts/validate-release-provenance-readiness.mjs
```

The report uses schema `covenant.release-provenance-readiness.v1`. It does not generate keys, sign releases, publish artifacts, or write transparency-log entries.

## Gates

| Gate | Current state | Evidence | Human boundary |
|---|---|---|---|
| `release-subject-schema` | Documented | `docs/provenance/release-subjects.md` | No publication decision. |
| `local-provenance-verifier` | Implemented | `agent-os/scripts/provenance.mjs` | No publication decision. |
| `audit-root-signing-policy` | Documented | `docs/decisions/0004-audit-root-signing-policy.md` | Project signing identity approval. |
| `audit-root-release-subject-binding` | Implemented | `agent-os/scripts/provenance.mjs`, `agent-os/scripts/provenance-self-test.mjs` | No signing key approval. |
| `review-artifact-signing-contract` | Documented | `docs/provenance/review-artifact-signing.md` | Project review key approval. |
| `project-key-custody` | Planned | Signing policy docs | Key custody, publication, rotation, and revocation. |
| `release-artifact-subject-verifier` | Planned | Subject schema docs | Implementation of release artifact digest verification. |
| `transparency-publication` | Planned | None yet | Publication target, credentials, and release evidence policy. |

`ready_for_local_release_provenance_planning` can be true while `ready_for_public_release_provenance` remains false. That is the expected state until public key custody, artifact-subject verification, and transparency publication exist.

## Artifact Subject Metadata

Release artifact subjects must bind:

- repository;
- release id;
- canonical commit;
- artifact names;
- SHA-256 artifact digests;
- byte counts;
- validation evidence.

Automation may prepare reports, fixtures, and validators. Humans still own project signing identity, public key publication, artifact publication, transparency-log target, and release evidence acceptance policy.
