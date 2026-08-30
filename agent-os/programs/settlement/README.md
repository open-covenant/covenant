# Covenant settlement program

This Anchor program contains two independent payment surfaces:

- the existing COVNT staking, credits, and task escrow instructions;
- Covenant Compute escrow funded in an administrator-configured six-decimal
  USDC mint.

COVNT is not required to fund or settle a compute job. This slice does not
define a COVNT discount, access tier, or staking requirement.

## Compute escrow lifecycle

The protocol administrator initializes the `compute_config` PDA with the
accepted USDC mint and a settlement authority. The authority can be rotated by
the protocol administrator.

`fund_compute_job` creates the canonical
`[b"compute_escrow", job_id, client]` PDA and deposits the client's maximum
authorized USDC amount. Scoping the seeds to the client keeps a job ID seen in
flight from being squatted. The account permanently binds:

- the job ID and quote commitment;
- the client and provider;
- the exact escrow vault, client refund, and provider payment token accounts;
- the USDC mint, maximum charge, and expiry.

`settle_compute_job` requires the current settlement authority. It rejects an
actual charge above the maximum, pays the provider, and returns the rest of the
authorized maximum to the client in the same Solana transaction. Payouts are
computed from the recorded maximum, never the live vault balance, so
`actual + refunded == maximum` holds even if a third party sends tokens to the
vault address.

Expiry is bounded on both ends. A client must fund at least
`MIN_COMPUTE_ESCROW_SECONDS` of runway, and settlement stays open for
`COMPUTE_SETTLEMENT_GRACE_SECONDS` past `expires_at`, so a client cannot pick an
expiry that consumes compute and then reclaims the deposit.

`refund_compute_job` returns the authorized maximum to the client. The
settlement authority can invoke it for a failed job at any time; the client can
invoke it once the grace window closes. Refund remains available while the
broader protocol is paused so a pause cannot strand deposited USDC. Replaying a
refund matches on the commitment alone, so an authority retry that lands after
the client's own refund succeeds instead of failing.

Settled and refunded accounts remain allocated with zero-balance vaults.
Repeating the exact terminal instruction succeeds without moving tokens.
Conflicting replays fail. Retaining the accounts prevents the same job ID from
being initialized again and provides durable outcome state; rent reclamation is
deliberately deferred.

## Status

The instruction and account lifecycle is implemented and exercised against the
compiled SBF program with LiteSVM. It is not evidence of a deployed compute
settlement service. The TypeScript SDK exposes staged low-level builders, but
requires callers to supply an explicit compute program ID, cluster, and RPC URL;
it never defaults these instructions to the existing mainnet settlement
deployment and rejects that current program ID on mainnet until compute support
is deployed there. Production use still requires a program deployment,
cluster-specific USDC configuration, protected settlement-authority operations,
and desktop/runtime funding integration.

## Validation

```bash
anchor build --program-name covenant_settlement_program
cargo test -p covenant-settlement-program --test compute_escrow
```
