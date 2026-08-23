# Signer, refund, and escrow incident recovery

The first rule is to preserve idempotency. A timed-out transaction may already be finalized. Never compensate with a manual transfer until the original operation has been reconciled on-chain and made terminal in the signer store.

## Severity and immediate containment

Treat these as P0: signer key exposure, transfer above policy, wrong recipient or mint, duplicate finalized transfer, inconsistent prepared transaction bytes, database loss, or an unexplained reserve shortfall.

For P0:

1. Set both durable admission controls to `false` with an authenticated `POST /v1/admin/admission` request and an incident-specific reason. Confirm `GET /v1/admission` reports both closed without terminating the API process needed for receipts.
2. Close updater promotion control through authenticated `PUT /v1/admin/promotion-control` and confirm the returned revision. Existing claimant PR submissions and disputes stay available; the claims switch blocks only new bindings.
3. Preserve API, signer, database, RPC, and webhook logs in read-only storage. Record UTC start time and the last known healthy commit.
4. Do not rotate or drain the signer wallet until submitted signatures and prepared transaction bytes are reconciled.
5. If key exposure is confirmed, reconcile first, then revoke network access, rotate the signer, and move only unencumbered funds to a new treasury under an explicit incident approval.

P1 covers signer unavailable, RPC unavailable, price oracle unavailable, refund pending beyond five minutes, escrow funding/release pending, or database connection exhaustion with no evidence of policy violation.

## Pause updater promotions

1. Read `GET /v1/admin/promotion-control` with an updater credential and record the current revision.
2. Send `PUT /v1/admin/promotion-control` with the write/admin token, `promotionsEnabled: false`, that `expectedRevision`, and an incident-specific reason. A `409` means another operator changed the control; read it again rather than overwriting it.
3. Wait for `200` and confirm the returned control is closed. Mutation, merge admission, and promotion admission share one database gate, so a successful response proves no merge or promotion hook is still in flight and neither can begin.
4. If the request reaches its timeout, assume a merge or promotion may be in flight. Keep the control closed, inspect the upgrade in `merging`, `promoting`, or `verifying_promotion`, and reconcile GitHub state plus the stable hook idempotency key with the deployment system. Do not submit a manual promotion or reopen control until the merge receipt, operation ID, and active production revision are known.
5. Do not stop updater recovery. A closed control blocks a new promotion call but deliberately preserves production-health monitoring and rollback for any candidate already promoted.

## Signer unavailable

1. Close paid intake through `POST /v1/admin/admission` and confirm the public status. Existing jobs may finish, but no new liability is accepted.
2. From the API private network, check `http://mizuki-policy-signer:8792/health`. A failure is not authorization to bypass the signer.
3. Check signer process status, PostgreSQL connectivity, RPC finality, price-source freshness, and the rolling-limit counters.
4. Restart only the signer service on the same healthy commit. Startup recovery must load non-terminal operations from PostgreSQL.
5. For each operation in `prepared`, `broadcasting`, `submitted`, or `reconciling`, compare persisted transaction bytes, idempotency key, recipient, amount, and observed signature. Do not rebuild a different transaction under the same operation.
6. Resume intake only after recovery makes all old operations terminal or leaves them in a documented, chain-observed reconciling state with no possible duplicate path.

## Refund pending or failed

1. Identify by job ID, settlement signature, policy operation ID, and original payment signature.
2. Verify the original payment at finalized commitment: payer, recipient, canonical mint, exact amount, and no prior successful refund.
3. Query the signer operation. If it has a signature, query that signature and the recipient token balance before retrying.
4. Retry the same refund request with the same job and settlement evidence. The signer must return or resume the same operation.
5. If the chain finalized but the database did not, let recovery reconcile the stored operation; do not broadcast again.
6. If the transaction expired without landing, recovery may rebuild only according to the signer's persisted-operation contract. Capture the old and new signatures under the same operation history.
7. Mark the customer refund complete only after finalized chain evidence. Create the rescue bounty only after that transition commits.
8. Publish incident duration and refund evidence. Exclude bearer tokens, private RPC URLs, and raw transaction signing material.

## Escrow funding pending

1. Keep the affected bounty assigned while an escrow operation is non-terminal. The 48-hour claim deadline is immutable; do not invite another claimant while the escrow operation is non-terminal.
2. Reconcile the escrow operation by bounty ID, claimant wallet, amount, acceptance hash, and expiry.
3. If funded on-chain, update state from finalized evidence. If not landed, retry the same idempotent operation.
4. Reopen the bounty only after the original escrow is terminal and any reserved funds are returned. Never create a second escrow for the same active claim.

## Escrow release pending

1. Verify the PR URL, repository, merge commit, merged-at time, acceptance hash, required checks, and independent review receipt.
2. Query the existing release operation and its signature. A webhook retry must not create another release.
3. If finalized, reconcile the database and publish the release receipt. If not landed, retry the same release operation.
4. Do not pay the contributor manually while an escrow release can still finalize.

## Escrow expiry or refund pending

1. Verify the immutable claim expiry and confirm no accepted merge exists.
2. Reconcile any submitted release before requesting an escrow refund.
3. Request refund through the signer once. Confirm finalized return to the treasury and one ledger entry.
4. Reopen the bounty only after the old claim and escrow are terminal. A new claimant receives a new acceptance hash and escrow operation.

## Database recovery

1. Put all services in maintenance mode and close updater promotion control using the procedure above.
2. Restore PostgreSQL to an isolated instance first. Never point production services at an unverified restore.
3. Compare signer operations against finalized chain history from before and after the backup timestamp.
4. Reconcile job, refund, bounty, escrow, and ledger tables in that order. Chain evidence wins for transfer finality; signed acceptance and merge receipts win for contributor release eligibility.
5. Run idempotency and balance checks, then shadow the restored stack before promotion.

## Exit criteria

- Every accepted customer liability is backed by reserve or a finalized refund.
- Every signer operation is terminal or explicitly reconciling against one known signature.
- Every custody movement in the ledger links to matching finalized chain evidence; model, sandbox, operating, and allocation entries remain explicitly non-custodial accounting records.
- No duplicate job, refund, bounty, escrow, release, or webhook side effect exists.
- Root cause, customer impact, timeline, remediation, and a regression test are published before intake resumes.
