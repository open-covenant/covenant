# Provenance

Covenant provenance envelopes connect an autonomy task, a Git commit, changed file evidence, transition history, and validation records.

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

## What Is Verified

- The subject commit exists in the local Git object database.
- The attested file list matches the subject commit diff.
- Every attested file blob and SHA-256 digest matches the subject commit.
- The task snapshot digest matches the task record stored in the subject commit.
- The task transition events match the event log stored in the subject commit.
- The envelope payload digest matches its contents.
- The envelope does not contain local home paths, personal email addresses, private SSH key names, or the Covenant SSH host alias.

## Current Limits

- Envelopes are not signatures.
- Envelopes are not transparency-log entries.
- Validation entries record evidence from the producing operator or automation; the verifier checks envelope consistency, not whether every command was re-run.
- Release artifact subjects are not included yet.
