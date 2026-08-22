# Public $2 successful-job canary

Objective: produce a public, independently inspectable chain from an operator-controlled issue to paid quote, validated PR, and merge before asking an external maintainer to assume first-run risk. Publish the measured variable execution estimate and every omitted cost category; do not claim gross margin until the complete commercial cost is durably recorded. This is a real mainnet payment, not demo data, but it is internal proof and never counts as external traction.

## Preconditions

- The GitHub App is installed on one operator-controlled public repository listed in `MIZUKI_INTERNAL_REPOS`.
- The issue is a Micro task with one observable acceptance criterion, no secrets, no generated lockfile churn, no dependency upgrade, and no security-sensitive code.
- Quote is exactly $2.00 USDC and has not expired.
- API, signer, updater, coding gateway, PostgreSQL, RPC, facilitator, sandbox, and model route have been healthy for 30 uninterrupted minutes.
- The signer independently reports enough finalized refund capacity for the full principal, while limits remain $25 per operation and $100 rolling 24 hours.
- No unresolved refund, escrow, or submitted signer operation exists.
- A screen recorder and public evidence note are ready before payment.

Abort if any precondition is false. Do not improvise around a signer, RPC, database, or GitHub failure.

## Execution

1. Record the issue URL, quoted acceptance criteria, repository default-branch SHA, quote ID, amount, route/model, and UTC time.
2. The operator pays the x402 quote from the dedicated canary wallet. Never copy its private key or seed phrase into a service, log, script argument, or recording.
3. Record the finalized payment signature and public job receipt. Confirm the payer, mint, amount, recipient, and finality independently on-chain.
4. Watch the job state through checkout, patch generation, repository checks, one permitted repair pass, and independent review. Do not edit state in PostgreSQL.
5. Confirm the PR is opened by the GitHub App against the pinned base SHA and links the paid issue. Verify diff scope, checks, review receipt, model route, token-rate estimate, measured sandbox-runtime estimate, and sandbox receipt.
6. Let the repository maintainer review normally. Mizuki must not merge his own PR.
7. After the maintainer merges, confirm webhook reconciliation records the merge exactly once.
8. Confirm the public dashboard shows the $2 inflow, model-token and measured sandbox-runtime estimates, current refund liability, and an explicit list of omitted commercial costs. Gross margin must remain `unverified` unless provider billing adjustments, chain/facilitator fees, and infrastructure costs are all durably recorded.
9. Publish one evidence bundle containing the issue, payment signature, job receipt, PR, checks, independent review result, merge commit, duration, variable execution estimate, omitted cost categories, and gross-margin status. Redact only secrets and non-public request headers.

## Pass criteria

- One finalized $2.00 USDC payment, one scoped PR, all required checks green, and one maintainer merge.
- No refund requested or emitted.
- The receipt distinguishes measured estimates from complete commercial cost and makes no unsupported margin claim.
- Every state transition and financial entry appears once despite webhook or operator retries.

If the canary fails after payment, stop treating it as the success canary. Follow `canary-refund-bounty.md` and the applicable incident path without hiding or reclassifying the failure.

Publish the receipt immediately, then run the first external-maintainer job. This canary contributes zero paid jobs, repositories, maintainers, PRs, or merges to the external traction gates.
