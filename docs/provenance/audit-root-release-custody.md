# Audit Root Release Custody

Audit-root attestations can already be generated and verified locally. Release use needs stricter subject binding and human-governed key custody before any public non-repudiation claim.

## Implemented Local Binding

`agent-os/scripts/provenance.mjs audit-root write` supports release targets:

```bash
node agent-os/scripts/provenance.mjs audit-root write \
  --report audit-report.json \
  --release v0.1.0-alpha.1 \
  --release-subject release-subject.json \
  --commit <commit> \
  --out docs/provenance/audit-roots/<commit>-audit-root.json \
  --validation "covenant audit verify=passed"
```

When `--release-subject` is supplied, the verifier checks:

- schema `covenant.provenance.release.v1`;
- `subject.kind: release_bundle`;
- repository matches the audit-root attestation repository;
- release id matches `--release`;
- commit is canonical and matches the audit-root subject commit;
- artifacts have stable names, SHA-256 digests, and byte counts;
- validation evidence is present;
- `target.releaseSubjectSha256` matches the embedded release subject.

This binds an audit root to a release subject without requiring a real project signing key.

## Custody Checklist

Before an audit-root attestation is treated as public release evidence, a human release operator must approve:

- project signing identity type: GitHub Actions OIDC/Sigstore or an offline project key;
- public key or identity publication location;
- key rotation and revocation procedure;
- release subject acceptance criteria;
- where signed audit roots are stored and announced;
- transparency-log publication target and retry policy.

Until those decisions are complete, signed audit-root attestations prove payload integrity for the embedded key only. They do not prove project custody.

## Validation

Run the provenance self-test:

```bash
node agent-os/scripts/provenance-self-test.mjs
```

The fixture generates an unsigned release-target audit-root attestation with an embedded release subject, verifies it, then tampers the release id and proves the verifier rejects the mismatch.
