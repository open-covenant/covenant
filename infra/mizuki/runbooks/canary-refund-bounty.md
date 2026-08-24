# Public forced refund-to-bounty canary

Objective: prove that a real paid maintenance failure returns 100% of principal and becomes a funded, externally claimable capability bounty. This is an adversarial canary, not a staged database edit.

Use a cooperating operator maintainer and a dedicated public Micro issue in a repository listed in `MIZUKI_INTERNAL_REPOS`. The controlled failure is loss of GitHub App access only after the API has finalized payment and registered the full refund liability. The maintainer removes the repository installation immediately after the paid job response, while isolated patch generation is still running. Mizuki's mandatory authorization recheck then blocks publication and enters the real refund path. Reinstall the App only after the refund is finalized and the bounty has been published. This is real mainnet evidence but does not count as external traction.

## Preconditions

- The maintainer understands and consents to the temporary App removal and public evidence.
- There are no other active paid jobs. Stop if normal intake cannot be held during the short canary window.
- The $2 quote, payer wallet, refund treasury, refund mint, signer limits, and expected rescue bounty amount are recorded.
- The signer, database, RPC, and price source have been healthy for 30 minutes. Do not combine this drill with infrastructure fault testing.
- A separately controlled contributor identity and wallet are ready to exercise claim, merge, and release. If the contributor is operator-controlled, label that fact publicly and do not count it as external adoption.
- The operator has rehearsed the post-payment removal timing on devnet. The issue must leave enough route time to remove access before publication; abort before payment if it does not.
- The operator has recorded the closed admission revision and verified the append-only audit endpoint.

## Failure and refund

1. Read the authenticated admission control, open only paid intake with its exact `expectedRevision`, retain the returned audit revision, and keep new claims closed. Record the public issue, quote ID, acceptance criteria, installation ID, authorization receipt hash, and UTC time. Keep the App installed and the authorization label unchanged.
2. The first maintainer pays exactly $2.00 USDC through x402 before the quote expires.
3. Wait for the API's paid job response. From the private operator view, confirm the settlement is finalized and the signer has registered the matching refund liability. Do not expose the liability bearer authorization or internal tokens.
4. Immediately remove the GitHub App installation from the target repository and record the removal time. Do not change the issue, label, branch, or acceptance criteria.
5. Confirm the pre-publication authorization recheck detects the missing installation. Mizuki must move the job to refund-pending and submit one policy operation using the registered settlement signature. Close paid intake with the current revision and retain the matching audit entry before continuing the refund drill.
6. The signer independently derives payer, mint, recipient, and amount from finalized chain data. It must not accept those facts from the API.
7. Confirm a finalized refund returns exactly $2.00 USDC principal to the original payer. Network fees are recorded separately and never subtracted from principal.
8. Confirm retrying the same refund intent returns the same operation and does not produce a second transfer.
9. Publish the payment signature, liability receipt ID, refund signature, operation ID, public job receipt, failure class, and elapsed time from terminal failure to final refund.

If the PR is published before App removal, the forced-failure canary did not run. Complete that paid job normally; never manufacture a failure or edit its durable state. Rehearse again and use a new issue and quote.

If refund finality is not reached within five minutes, stop the stream, mark the canary failed, and use `incident-recovery.md`. Never send an ad-hoc treasury transfer while the original policy operation may still finalize.

## Bounty and capability

1. Confirm refund finalization creates one rescue bounty linked to the failed job and one capability proposal linked to the failure class.
2. Confirm the bounty amount follows policy: the larger of $10 or twice the failed job price, capped at $25. For this canary the expected amount is $10.00.
3. Confirm the signer reserves the exact SOL principal in the on-chain escrow once, and that the bounty publishes the finalized funding signature and atomic amount before becoming `open`. If signer-reported SOL capacity is insufficient, the bounty must remain awaiting funding and the canary fails. Replenish the dedicated escrow authority through `escrow-capacity.md`, then resume the same durable operation; never patch bounty or ledger state directly.
4. The first maintainer reinstalls the GitHub App with only the target public repository selected.
5. Read the authenticated admission control, confirm paid intake remains closed, then open only new claims with its exact `expectedRevision` and retain the audit revision.
6. The canary contributor authenticates with GitHub, proves control of a contributor wallet, and claims the bounty. Record the immutable 48-hour deadline.
7. Confirm the private signer creates one contributor escrow only after the claimant wallet and acceptance hash are fixed.
8. The claimant submits a scoped PR. Mizuki runs repository checks and an independent review; the claimant cannot self-approve.
9. The repository maintainer merges the accepted PR. Confirm the merge receipt releases the contributor escrow exactly once.
10. Close both admission controls with the current revision and retain the final audit entry. Publish the complete failure-to-capability chain: admission audit revisions, payment, failure, full refund, bounty, claim, escrow, PR, review, merge, release, capability record, variable execution estimate, omitted commercial costs, and gross-margin status.

## Pass criteria

- Principal refund success is 100%, with no duplicate transfer.
- Exactly one rescue bounty and one capability proposal exist for the failed job.
- A separately controlled contributor claims, ships, and receives the bounty after maintainer merge; any operator affiliation is disclosed.
- No signer key or internal bearer token appears in logs, stream footage, receipts, or screenshots.

After the pass, reopen normal intake only when all signer operations are terminal and the dashboard's signer-derived refund and escrow custody snapshot reconciles to finalized chain evidence.

Publish the complete receipt immediately. None of this canary's job, repository, maintainer, PR, merge, or contributor counts toward the external traction gates unless an independently controlled external participant actually performed that role.
