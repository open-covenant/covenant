# Commercial-core alerts

Scrape the public API `/metrics` and the private signer `/metrics` every 30 seconds. Page the operator on every critical rule below; do not wait for a customer report. The API's durable admission switch is the first incident action.

## Critical rules

| Signal                                                                                         | Trigger                        | Immediate action                                                                                                                                        |
| ---------------------------------------------------------------------------------------------- | ------------------------------ | ------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `mizuki_refund_protection_verified`                                                            | Not `1` for 60 seconds         | Close paid intake and new bounty claims. Check signer readiness, both RPC views, treasury coverage, and application-to-signer liability reconciliation. |
| `mizuki_refund_liability_reconciled`                                                           | Not `1` for 60 seconds         | Close paid intake. Reconcile every settled job before any manual transfer or liability discharge.                                                       |
| `mizuki_settlement_pending_oldest_seconds`                                                     | Greater than 120               | Close paid intake and invoke settlement recovery for the exact persisted reservation. Escalate at 15 minutes.                                           |
| `mizuki_refund_pending_oldest_seconds`                                                         | Greater than 300               | Close paid intake. Inspect the durable signer operation and retry recovery; never create a second transfer. Escalate at 15 minutes.                     |
| `mizuki_signer_operations{status=~"reserved\|prepared\|broadcasting\|submitted\|reconciling"}` | Greater than `0` for 5 minutes | Close intake and claims. Reconcile the existing operation by its resource key and signed bytes.                                                         |
| `mizuki_signer_errors_total`                                                                   | Any increase                   | Close intake if the error touches settlement, liability, refund, or escrow. Preserve the request, operation, and audit identifiers.                     |
| `mizuki_refund_success_ratio`                                                                  | Below `1` when defined         | Close paid intake immediately and treat it as a financial incident.                                                                                     |
| `mizuki_bounties_unfunded_open`                                                                | Greater than `0`               | Close claims, remove the public offer, and reconcile escrow creation.                                                                                   |

## Warning rules

- Page at 50% and 80% utilization of signer refund or escrow daily limits.
- Warn when new-intake refund capacity falls below the 10 USDC Standard principal; stop Standard quotes before it reaches zero.
- Warn on any coding route, reviewer route, sandbox, GitHub App, updater, Postgres, or signer readiness failure. Paid intake must already fail closed while readiness is unhealthy.
- Warn when no scrape succeeds for 90 seconds. Missing evidence is an incident, not a healthy zero.
- Warn when a failed paid job has no capability handoff or when a refunded job has no funded-bounty attempt after five minutes.

## Drill evidence

For every launch rehearsal, retain the alert timestamp, the matching row from authenticated `GET /v1/admin/admission/audit`, affected job and signer operation IDs, both RPC observations, recovery result, and the time protection returned to verified. A drill passes only if new intake closes before a second payment can settle and the original operation reaches one terminal state without a duplicate economic effect.

Do not include payer addresses, credentials, signed transaction bytes, or private repository data in alert labels.
