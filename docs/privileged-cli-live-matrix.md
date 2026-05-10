# Privileged CLI Live Matrix

The privileged CLI matrix records which daemon-backed commands have opt-in live coverage and which commands are intentionally deferred. It prevents broad status claims such as "all privileged CLI verbs are covered" unless the command-level evidence exists.

The machine-readable source is `agent-os/autonomy/privileged-cli-live-matrix.json`. Validate it with:

```bash
node agent-os/scripts/validate-privileged-cli-live-matrix.mjs
```

Rows use two states:

- `covered`: at least one ignored `live_*.rs` test exercises the command through the real daemon or CLI boundary.
- `deferred`: the command exists as a public CLI surface, but live coverage is intentionally blocked by a missing implementation boundary or external prerequisite.

Current deferred commands are the Solana transaction-preparation verbs:

- `chain register-agent`
- `chain stake`
- `chain buy-credits`

Those commands are not daemon-signed settlement operations yet. They must remain deferred until daemon-side signing, deployment readiness, and custody policy exist.

The next useful matrix expansion is not more table prose. Add a live test for the highest-risk deferred or newly added privileged verb, then update the matrix in the same change.
