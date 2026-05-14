# Provenance

Covenant provenance envelopes connect an autonomy task, a Git commit, changed file evidence, transition history, and validation records. The same verifier also understands unsigned audit-root attestations that bind a local audit integrity report to a commit and task or release target.

The envelope format is intentionally simple JSON. It is designed for public inspection and CI verification as release signing and transparency publication mature.

## Commands

Generate an attestation:

```bash
node agent-os/scripts/provenance.mjs write \
  --task memory-drift-repair \
  --commit 20ff55e \
  --out docs/provenance/attestations/20ff55e-memory-drift-reports.json \
  --validation "bash agent-os/scripts/validate.sh --quick=passed"
```

Verify one attestation:

```bash
node agent-os/scripts/provenance.mjs verify \
  --file docs/provenance/attestations/20ff55e-memory-drift-reports.json
```

Verify every committed attestation:

```bash
node agent-os/scripts/provenance.mjs verify-all
```

`agent-os/scripts/validate.sh` runs `verify-all` automatically.

Autonomy review artifacts are generated unsigned by default. The verifier can check signed review artifacts when an approved project public key is supplied; project key custody and publication are defined in [keys/](keys/).
Release provenance readiness is gated on artifact subject verification, project key custody, and transparency publication; only the first two have landed contracts so far, and public release provenance remains blocked until the first key is published and the transparency target is selected.

Generate an audit-root attestation from `covenant audit verify` output:

```bash
covenant audit verify > audit-report.json

node agent-os/scripts/provenance.mjs audit-root write \
  --report audit-report.json \
  --task audit-root-attestation-v1 \
  --commit HEAD \
  --out docs/provenance/audit-roots/<commit>-audit-root.json \
  --validation "covenant audit verify=passed"
```

Bind a release-target audit root to a release subject:

```bash
node agent-os/scripts/provenance.mjs audit-root write \
  --report audit-report.json \
  --release v0.1.0 \
  --release-subject release-subject.json \
  --commit HEAD \
  --out docs/provenance/audit-roots/<commit>-audit-root.json \
  --validation "covenant audit verify=passed"
```

Bind a release-target audit root to both a release subject and a release scope manifest (see [release-scopes.md](./release-scopes.md)):

```bash
node agent-os/scripts/provenance.mjs audit-root write \
  --report audit-report.json \
  --release v0.1.0 \
  --release-subject release-subject.json \
  --release-scope release-scopes/v0.1.0.json \
  --commit HEAD \
  --out docs/provenance/audit-roots/<commit>-audit-root.json \
  --validation "covenant audit verify=passed"
```

A single signature over the resulting audit-root payload then covers both the release artifact set (via `releaseSubjectSha256`) and the in-scope autonomy task set (via `releaseScopeSha256`).

Add a detached ed25519 signature when a project signing key is available:

```bash
node agent-os/scripts/provenance.mjs audit-root write \
  --report audit-report.json \
  --task audit-root-attestation-v1 \
  --commit HEAD \
  --out docs/provenance/audit-roots/<commit>-audit-root.json \
  --signing-key ./secure/project-audit-root-key.pem \
  --key-id covenant-root \
  --validation "covenant audit verify=passed"
```

Verify one audit-root attestation:

```bash
node agent-os/scripts/provenance.mjs audit-root verify \
  --file docs/provenance/audit-roots/<commit>-audit-root.json
```

## What Is Verified

- The subject commit exists in the local Git object database.
- The attested file list matches the subject commit diff.
- Every attested file blob and SHA-256 digest matches the subject commit.
- The task snapshot digest matches the task record stored in the subject commit.
- The task transition events match the event log stored in the subject commit.
- The envelope payload digest matches its contents.
- The envelope does not contain local home paths, personal email addresses, private SSH key names, or the Covenant SSH host alias.

For `covenant.audit-root-attestation.v1`, the verifier also checks:

- The audit integrity report is valid.
- Event and anchor counts match.
- The root hash is lowercase 64-character hex.
- The report has no failure diagnostics.
- The subject commit is canonical.
- Task targets match the task snapshot stored in the subject commit.
- Release targets with `releaseSubject` match repository, release id, commit, artifact metadata, validation evidence, and `releaseSubjectSha256`.
- Release targets with `releaseScope` (a [`covenant.release-scope.v1`](./release-scopes.md) manifest) match repository, release id, and commit, and the embedded `releaseScopeSha256` matches a recomputed digest of the canonicalized scope payload.
- Unsigned signing blocks are explicitly unsigned.
- Signed blocks use ed25519, include canonical SPKI public key material, match the public-key digest, and verify against the canonical payload with `signing.signature` cleared.

## Current Limits

- Commit provenance envelopes are not signatures.
- Envelopes are not transparency-log entries.
- Signed audit-root attestations prove payload integrity for the embedded public key. Public trust still requires a project-controlled key policy, release process, and transparency publication.
- Validation entries record evidence from the producing operator or automation; the verifier checks envelope consistency, not whether every command was re-run.
- Release artifact subject schema is defined in docs/provenance/release-subjects.md. Audit-root release targets can bind an embedded release subject digest; produced artifact file verification remains planned.
- Review artifact signing has verifier support behind an explicit trusted public key input. The project signing key custody contract, publication location, manifest schema, rotation policy, authorized-signer role, and release-evidence floor are documented; no project key has been published yet. See [keys/](keys/) for the publication location and manifest.
- Release provenance readiness has a read-only gate report; public release provenance remains blocked until the first key is published, release-artifact subject verification is implemented, and a transparency publication target is selected.

Audit root signing is implemented as detached `audit-root-attestation.v1` payloads with optional local ed25519 signatures. Release publication and transparency-log publication remain planned work, gated on the same project signing key publication step recorded in [keys/](keys/).
