# Audit-root attestations

Operator-published audit-root attestations land here as
`<commit>-audit-root.json`, produced via:

```bash
covenant audit verify > audit-report.json

node agent-os/scripts/provenance.mjs audit-root write \
  --report audit-report.json \
  --task audit-root-attestation-v1 \
  --commit <commit-sha> \
  --out docs/provenance/audit-roots/<commit-sha>-audit-root.json \
  --validation "covenant audit verify=passed"
```

For release-target attestations bind `--release <tag>`, `--release-subject <path>`,
and optionally `--release-scope <path>` instead of `--task`. The full flag set,
signing variant, and verification steps are documented in
[`../README.md`](../README.md); see [`../../audit-integrity.md`](../../audit-integrity.md)
for the audit-report contract.
