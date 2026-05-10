# On-Chain Settlement Readiness

The settlement program is scaffolded, and local receipts can be recorded and batched. That is not deployment readiness.

Run the read-only readiness report from the repository root:

```bash
node agent-os/scripts/settlement-deployment-readiness.mjs --json
```

Validate the report contract:

```bash
node agent-os/scripts/validate-settlement-deployment-readiness.mjs
```

The report uses schema `covenant.settlement-deployment-readiness.v1`. It does not deploy programs, change mint authorities, select oracles, write chain state, or accept a security review.

## Gates

| Gate | Current state | Evidence | Human boundary |
|---|---|---|---|
| `program-scaffold` | Implemented | `agent-os/programs/settlement` | No deployment decision. |
| `local-receipt-ledger` | Implemented | `agent-os/crates/covenant-settlement` | No deployment decision. |
| `deployment-runbook` | Documented | `docs/on-chain-settlement-readiness.md` | No deployment decision. |
| `security-review` | Planned | None yet | Review scope, acceptance, and remediation approval. |
| `oracle-policy` | Documented, source selection blocked | `docs/settlement-oracle-policy.md`, `agent-os/scripts/settlement-oracle-policy.mjs`, `agent-os/scripts/validate-settlement-oracle-policy.mjs` | Oracle source selection, update authority, freshness, manipulation, outage, and deployment binding. |
| `mint-authority-policy` | Planned | None yet | Mint authority, treasury custody, and rotation approval. |
| `emergency-operations` | Planned | Program pause surface only | Pause, rollback, redeploy, signer quorum, and incident authority. |

`ready_for_local_scaffold` can be true while `ready_for_onchain_deployment` is false. That is the expected state until review, oracle, mint authority, treasury, and emergency-operation evidence exists.

## Deployment Sequence

Before deployment, the operator should have:

- accepted security review evidence tied to the exact program commit;
- a deployment target, upgrade authority, and rollback plan;
- oracle source, update authority, freshness, manipulation, outage, and deployment-binding evidence;
- mint authority custody, treasury ownership, and rotation records;
- emergency pause authority, redeploy sequencing, and signer quorum;
- local and on-chain validation commands with pass/fail/skipped outcomes.

Automation may prepare reports and validation evidence. Humans still own deployment approval, security-review acceptance, oracle selection, mint authority changes, treasury custody, and emergency operations.
