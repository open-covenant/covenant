# Provenance

Covenant provenance envelopes connect an autonomy task, a Git commit, changed file evidence, transition history, and validation records. The same verifier also understands unsigned audit-root attestations that bind a local audit integrity report to a commit and task or release target.

The alpha format is intentionally simple JSON. It is designed for public inspection and CI verification before the project introduces release signing or transparency-log publication.

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

Generate an unsigned audit-root attestation from `covenant audit verify` output:

```bash
covenant audit verify > audit-report.json

node agent-os/scripts/provenance.mjs audit-root write \
  --report audit-report.json \
  --task audit-root-attestation-v1 \
  --commit HEAD \
  --out docs/provenance/audit-roots/<commit>-audit-root.json \
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
- The unsigned signing block has not been replaced by an unsupported signature claim.

## Current Limits

- Envelopes are not signatures.
- Envelopes are not transparency-log entries.
- Audit-root attestations are generated and verified, but they are unsigned until a project signing identity is selected.
- Validation entries record evidence from the producing operator or automation; the verifier checks envelope consistency, not whether every command was re-run.
- Release artifact subjects are not included yet.

Audit root signing is planned separately in [ADR 0004](../decisions/0004-audit-root-signing-policy.md). The current implementation defines and verifies the detached `audit-root-attestation.v1` payload, while project signing and transparency-log publication remain planned work.
