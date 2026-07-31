# Multichain value capture

Covenant projects selected registrations, score schemas, and signed statements
to other chains — alongside x402 payments over EIP-3009 USDC — without issuing
a token on any of them. A signature proves that bytes were signed by the
configured key; publisher attribution still depends on the trusted mapping to
that key, and the signature does not prove the underlying claim. `$CVNT` stays
Solana-canonical. This document records how the token accrues value under that
constraint and which parts of the model exist in code today versus which
require on-chain upgrade authority and are deferred.

## The invariant

1. **`$CVNT` is Solana-only.** The canonical mint
   `2mNVZ6aEjrGwiUVCfz7XGWpiXuWzgBDoznwE579upump` is never bridged, wrapped, or
   re-minted on another chain. No Wormhole NTT, no LayerZero OFT, no xERC20
   lockbox, no `wCVNT`.
2. **The Solana/Base evidence payment surfaces use chain-local USDC.** The x402
   sellers and outbound formats described in this document use EIP-3009 USDC on
   Base and SPL USDC on Solana. This is not a repository-wide denomination rule:
   other integrations can use their own settlement assets. No per-call path may
   bridge or wrap `$CVNT`.
3. **The Solana/Base bond format is chain-local USDC.** A configured bond on
   these evidence surfaces is stated in the USDC of the chain where its receipt
   is consumed (`covenant-attestation`'s signed bond statement). The signature
   proves possession of the configured key; consumers must independently pin
   that key, verify funding, and apply their own policy.

The invariant has a tripwire, not just prose:
[`agent-os/scripts/validate-cvnt-solana-quarantine.mjs`](../agent-os/scripts/validate-cvnt-solana-quarantine.mjs)
runs in `validate.sh --scripts` and fails the build when the canonical mint's
literal address, a named off-Solana bridge primitive (Wormhole NTT, LayerZero
OFT, xERC20, a `wCVNT`), or a payment surface that has dropped its USDC
denomination appears in a cross-chain crate (`covenant-attestation`,
`covenant-evm-signer`, `covenant-x402`, `covenant-x402-signer`, `agent-os/evm`).
It carries an always-on self-test (`--self-test`) proving each detector fires on
a known-bad fixture while staying quiet on the shipped prose — these crates
advertise "no bridge required" and name the Solana-side `sap-bridge`, and neither
is a violation.

It is a tripwire for the obvious accidental introduction, not a proof of the
invariant: the mint check is a literal-string match, so a mint passed via config
or an unnamed bridge SDK would need the human review the security gate already
requires. New cross-chain crates must be added to the guard's root list to be
covered. The strongest guarantee remains that you cannot denominate or bridge
`$CVNT` without naming the mint the guard watches for.

## How `$CVNT` captures value

The repository contains Solana-side fee-routing machinery. Cross-chain revenue
routing into it is not implemented.

### Implemented in source: Solana buy-and-lock routing

`agent-os/programs/stake` locks `$CVNT` for 7/30/90/180 days at
0.5×/1.0×/1.5×/2.0× weight and includes a permissioned `FeeRouter` and
`BuyLockVault` path for deposited Solana-side revenue. The code does not prove
that a named fee source continuously funds the router or guarantee a return to
lockers. There is currently no implemented Base-or-other-chain USDC route into
the Solana vault.

The first-party `agent-os/programs/stake` program has been sunset — early exit is
enabled and principal is withdrawable at any time regardless of the original lock —
and the current live staking program runs at
[stake.opencovenant.org](https://stake.opencovenant.org).

### Planned (cross-chain and onchain, escalation-gated)

The following extend the same buy-and-lock / real-yield design and require a
Solana program upgrade under owner authority. They inherit the staging rule from
`multichain-32` — build and dry-run only, no mainnet submission — and are not
implemented yet:

- **Operator / verifier stake.** Running the attestation oracle and the Base
  relayer requires a slashable `$CVNT` stake, so cross-chain attestation
  integrity is bonded on Solana. Misbehavior burns stake.
- **Premium bond tier.** An opt-in tier would let an agent post a bond in
  `$CVNT` for a policy-defined weight and staking yield, alongside — never
  replacing — the default USDC bond. Enrollment is a one-time Solana action; it
  must never introduce a Solana touch into a per-call hot path.
- **Governance.** The stake program exposes no governance surface today
  (it is denomination-isolated from settlement and reads no settlement state).
  A future governance surface would let locked `$CVNT` steer protocol
  parameters.
- **Cross-chain fee routing.** A future reconciler may convert or bridge
  chain-local protocol revenue into an explicitly governed Solana-side deposit.
  It must define custody, finality, accounting, failure recovery, and upgrade
  authority before any Base or other-chain fee is described as feeding the
  vault.

Until those land, the honest claim is narrow: the repository has Solana-side
buy-and-lock machinery, while multichain usage does not automatically accrue
value to `$CVNT`. The quarantine guard is a tripwire for named bridge patterns
and the known mint; it does not prove that every future integration preserves
the invariant.
