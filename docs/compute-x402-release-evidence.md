# x402 inference release evidence

Release candidate based on Compute deployment revision
`983172ddd04ac2de70136125e2088f22c6edf505`. This is a separate service; the
coordinator, Solana USDC economics, CVNT staking and existing fees are unchanged.
It uses the pinned official x402 V2 Solana implementation and a real Coinbase
AgentKit signing provider. Compatibility does not imply a commercial partnership.

## Local implementation

- Rust seller persists payment authorization, native signed job, verified result
  and settlement state before external actions. Original payment/job IDs survive
  retries and restarts. Finalized exact-transaction readback releases the result.
- The private verifier uses `@x402/svm` and `@x402/core` 2.28.0. Request-specific
  memo binding is signed by the standard client. Fee sponsorship is bounded.
- The native buyer enforces a durable spend budget and an approved operator set.
  The seller's lifetime reserve retains failed and uncertain operator costs.
- The AgentKit 0.10.4 client invokes a real registered action and wallet signature,
  checks merchant/network/asset/operator/request/output/receipt bindings, and keeps
  durable cumulative payment reservations. It is not an autonomous planning demo.
- The provider adapter has private loopback access, fixed model/input/output
  bounds, provider price ceilings and persistent reservations. Unknown requests
  cannot be automatically retried or assigned a fresh authorization.

The Node24 client has passed source and clean packed-package tests with actual
AgentKit signatures, the official verifier and a native Rust receipt fixture.
Focused Rust gateway, Node verifier, provider concurrency/restart and existing
OpenAI executor tests passed. Exact counts can change as acceptance findings are
fixed; these are local results, not a live purchase claim.

## Production evidence

The new operator's stake was explicitly approved and submitted on Solana mainnet:

- Operator: `9fefASd52qfR2Q8P81yXtkHu9E4q9KvW58x2ixBAAvLR`
- Stake: 1,000,000 CVNT; 30 days plus the CLI's ten-minute clock tolerance.
- Position: `5eA1C663UaWNrPaSCNXciAcCyxZDn3fopouK8bDp7pMF`
- Lock expiry: 2026-11-17 00:01 UTC.
- [Confirmed stake transaction](https://solscan.io/tx/nNT9c5vT2NdGWNPR8nZdm8RNNbc1ezApkg89S6Mgb2N4sC8UfD1ow35XfbrbVM1L5JqSyca6WGEpqejpqcivSUj)

Provider qualification passed for the exact canonical model `claude-haiku-4.5`
on 2026-10-18. The response returned the requested marker, used 15 input and
8 output tokens, and completed in 1.194 seconds. Two earlier requests using a
dated model identifier failed strict response-model validation. Their full
0.600480 USDC combined reservations remain counted as immutable audited losses;
the original request identities can never be retried. The successful call counts
83 micro-USDC at the configured price ceilings. No customer payment was involved.

The dedicated gateway buyer was funded with 3 USDC and its credited balance was
read back. The AgentKit test wallet holds 0.5 USDC; its associated token account,
the merchant token account and a separately bounded fee-payer allowance are ready.
The first immutable release image passed anonymous manifest and layer access.

Not yet evidenced: persistent service deployment, live inference matching,
funded AgentKit payment, returned verified result, restart replay, public launch
page/video and deployed sharing-card checks. Public admissions remain disabled
until those gates pass.

Deployment and budget allocation: [release runbook](../deploy/compute-x402/README.md).
