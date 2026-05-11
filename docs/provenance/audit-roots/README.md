# Audit-root attestations

Operator-published audit-root attestations land here as
`<commit>-audit-root.json`, produced via:

```
node agent-os/scripts/provenance.mjs sign-audit-root \
  --commit <commit-sha> \
  --out docs/provenance/audit-roots/<commit-sha>-audit-root.json
```

See [`../README.md`](../README.md) and [`../../audit-integrity.md`](../../audit-integrity.md)
for the verification flow.
