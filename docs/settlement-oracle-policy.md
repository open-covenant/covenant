# Settlement Oracle Policy

This document defines the readiness contract for settlement oracle and pricing policy. It does not select a production oracle, assign update authority, deploy accounts, or write chain state.

Run the read-only report from the repository root:

```bash
node agent-os/scripts/settlement-oracle-policy.mjs --json
```

Validate the report contract:

```bash
node agent-os/scripts/validate-settlement-oracle-policy.mjs
```

The report uses schema `covenant.settlement-oracle-policy.v1`.

## Readiness Boundary

`ready_for_policy_review` means the local repository has enough structured evidence for humans and agents to review the oracle policy requirements. It does not mean the on-chain oracle is ready.

`ready_for_onchain_oracle` must remain false until the human-owned decisions below are recorded, reviewed, and bound to a release candidate.

## Required Decisions

| Requirement | Required before on-chain readiness | Current state |
|---|---|---|
| Production source selection | Primary and fallback oracle sources, source ownership, licensing, and dependency provenance. | Human-owned blocker. |
| Update authority | Signer or signer quorum, custody, rotation, and revocation procedures. | Human-owned blocker. |
| Freshness and staleness | Maximum accepted price age, stale-data rejection, fallback behavior, and retry cadence. | Human-owned blocker. |
| Manipulation controls | Deviation thresholds, circuit-breaker rules, source manipulation assumptions, and review evidence. | Human-owned blocker. |
| Outage behavior | Pause, retry, resume, and operator escalation behavior during oracle outage. | Human-owned blocker. |
| Deployment binding | Oracle account, publisher, program configuration, and release evidence binding. | Human-owned blocker. |

## Acceptance Criteria

On-chain oracle readiness requires evidence that:

- the selected source and fallback source are named with dependency provenance;
- update authority custody and rotation are recorded;
- stale-data and outage behavior are tested against the selected source shape;
- manipulation thresholds and circuit breakers are reviewed;
- deployment configuration is bound to a release candidate;
- the settlement deployment readiness report includes the accepted policy evidence.

Autonomous agents may prepare reports, validators, fixtures, and candidate policy drafts. Human operators retain authority over production oracle source selection, update-key custody, deployment binding, and final release acceptance.
