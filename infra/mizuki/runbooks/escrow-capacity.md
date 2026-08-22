# Escrow capacity funding

This procedure adds SOL capacity to Mizuki's dedicated signer-controlled bounty authority. It never changes bounty state, creates a ledger deposit, or moves customer USDC. A bounty becomes funded only when the signer later creates and finalizes its exact on-chain escrow operation.

## Contain

1. Close paid intake and new claims through the durable API admission control. Close updater promotion control as a separate precaution.
2. Record the current finalized SOL balance, signer-reported available escrow reserve, outstanding escrow operations, and required bounty principal in lamports.
3. Confirm the escrow authority is the exact public key pinned in the signer configuration and registered as the ClawPump payout wallet. Never obtain or export the signer's private key.
4. Query the authority through two unrelated RPC providers at finalized commitment. Stop on any balance, program, or finality disagreement.

## Fund

1. Calculate the incoming amount from the required bounty principal, state/vault/guard rent, and configured fee reserve. Keep canary funding deliberately small; do not raise signer policy limits to make the transfer fit.
2. Send SOL from an operator-approved funding wallet to the public escrow authority using the project's normal hardware-wallet or multisig procedure. This is an incoming capacity transfer, not a bounty payment.
3. Record the funding signature, source approval reference, destination, atomic amount, and UTC time. Do not record seed phrases, private keys, bearer tokens, or private RPC URLs.
4. Wait for finalized commitment on both RPC providers. Confirm the destination's balance increased by the exact transferred amount.

## Reconcile

1. Refresh signer readiness. Its finalized escrow balance and available reserve must match the independently observed balance after rent and fee overhead.
2. Resume the existing durable bounty-funding operation. Never create a new bounty or idempotency key to work around a pending operation.
3. Confirm the bounty stays non-public or `awaiting_funding` until its distinct escrow transaction finalizes. The incoming capacity transfer is not proof that any specific bounty is secured.
4. Publish both receipts with different labels: capacity funding and bounty escrow funding. ClawPump earnings reports remain platform accounting unless a matching finalized distribution transaction is independently verified.
5. Reopen claims and intake only after every signer operation is terminal, signer-derived refund protection remains healthy, and the public custody snapshot agrees with both RPC views.

## Abort conditions

Stop immediately on a wrong destination, unexpected outgoing transfer, RPC disagreement, unexplained balance delta, signer/program hash mismatch, or any request to alter database or ledger state manually. Follow `incident-recovery.md`; do not compensate while a submitted transfer can still finalize.
