# Multichain value capture

Covenant's trust layer reaches other chains — ERC-8004 identity and EAS
reputation on Base, x402 payments over EIP-3009 USDC — without issuing a token
on any of them. `$CVNT` stays Solana-canonical. This document records how the
token accrues value under that constraint and which parts of the model exist in
code today versus which require on-chain upgrade authority and are deferred.

## The invariant

1. **`$CVNT` is Solana-only.** The canonical mint
   `2mNVZ6aEjrGwiUVCfz7XGWpiXuWzgBDoznwE579upump` is never bridged, wrapped, or
   re-minted on another chain. No Wormhole NTT, no LayerZero OFT, no xERC20
   lockbox, no `wCVNT`.
2. **Per-call payments are chain-local USDC.** Every metered call settles in the
   USDC of the chain it runs on, via x402 (`covenant-x402`: EIP-3009 on EVM, SPL
   on Solana). A per-call path denominated in `$CVNT` would force the token onto
   other chains and break the gasless facilitator flow, so it is prohibited.
3. **Bonds are chain-local USDC.** An agent's default trust bond is posted in the
   USDC of the chain the counterparty verifies on (`covenant-attestation`'s bond
   receipt). You do not secure a token with itself.

The invariant has a tripwire, not just prose:
[`agent-os/scripts/validate-cvnt-solana-quarantine.mjs`](../agent-os/scripts/validate-cvnt-solana-quarantine.mjs)
runs in `validate.sh --scripts` and fails the build when the canonical mint's
literal address, a named off-Solana bridge primitive (Wormhole NTT, LayerZero
OFT, xERC20, a `wCVNT`), or a payment surface that has dropped its USDC
denomination appears in a cross-chain root (`covenant-attestation`,
`covenant-evm-signer`, `covenant-x402`, `covenant-x402-signer`,
`covenant-x402-signer-evm`, `covenant-ens-gateway`, `covenant-evm-firewall`,
`covenant-identity`, `covenant-hyre`, `covenant-zauth`, `agent-os/evm`). The
Solana-native `programs/` tree is additionally scanned for bridge primitives
only — the canonical mint is legitimate at home, but bridging FROM Solana is
still bridging.
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

## Three reputation surfaces

Three different numbers answer "reputation" in this codebase, from three
different sources, measuring three different things. They are deliberately
named apart so no consumer can mistake one for another, and
[`validate-reputation-score-distinction.mjs`](../agent-os/scripts/validate-reputation-score-distinction.mjs)
fails the build if the names or this table drift:

| Surface | Where | Scale | Provenance | Audience |
| --- | --- | --- | --- | --- |
| Compliance score | `AuditReputation` (`covenant-audit/src/reputation.rs`) | 4-decimal fixed point (`SCORE_DECIMALS = 4`; `9500` = 0.95), `None` on a blank history | Recomputed from the agent's own tamper-evident audit chain: compliant actions over scored actions | Attestation consumers. **The only score projected to EVM** — it becomes the EAS `reputation-attest.v1` attestation on Base, built by `covenant-evm-signer` |
| Escrow standing | `EscrowReputation` (`covenantd/src/reputation.rs`) | Basis points (`completion_rate_bps`, 0..=10000), zero when the worker has no proofs | Escrow completion-proof and release rows in the audit chain, pinned to the chain root they were computed over | Marketplaces combining a worker's standing with earnings, over IPC (`GetReputation`). Solana/IPC-local; never projected to another chain |
| SAP peer score | `sap_reputation_score`, wire name `reputationScore` (`covenant-sap-bridge`) | Upstream-defined `u32`; SAP does not document the scale and Covenant does not assume one | Decoded from SAP's on-chain agent accounts by the bridge worker; nothing on the Covenant side audits how it was produced | Peer discovery display only. Must never feed a trust decision |

The first two are numerically treacherous precisely because their encodings
coincide: both express a 0..1 ratio in units of 1/10&#8308; (`6667` decodes to
`0.6667` under either reading), so swapping a compliance fraction for a
completion rate corrupts a trust decision with no numeric tell at all. The
distinct type names, not the scales, carry the distinction. The third has no
defined relationship to either; the bridge therefore carries the SAP score
under its own name and treats it as opaque metadata.

## How `$CVNT` captures value

Value accrues on Solana from cross-chain usage, without the token leaving it.

### Implemented: buy-and-lock real yield

`agent-os/programs/stake` locks `$CVNT` for 7/30/90/180 days at
0.5×/1.0×/1.5×/2.0× weight and pays lockers pro-rata SOL from protocol revenue —
pump.fun creator fees, the sandbox metered tier, Hyre markup, and SAP
capabilities — routed through a permissioned `FeeRouter` PDA. USDC per-call fees
earned on Base or any other chain feed the same revenue funnel that buys `$CVNT`
and fills the `BuyLockVault`; the multichain reach widens the fee base without
adding a second token. This is real yield from real usage, not emissions.

The first-party `agent-os/programs/stake` program has been sunset — early exit is
enabled and principal is withdrawable at any time regardless of the original lock —
and the current live staking program runs at
[stake.opencovenant.org](https://stake.opencovenant.org).

### Planned (on-chain, escalation-gated)

The following extend the same buy-and-lock / real-yield design and require a
Solana program upgrade under owner authority. They inherit the staging rule from
`multichain-32` — build and dry-run only, no mainnet submission — and are not
implemented yet:

- **Operator / verifier stake.** Running the attestation oracle and the Base
  relayer requires a slashable `$CVNT` stake, so cross-chain attestation
  integrity is bonded on Solana. Misbehavior burns stake.
- **Premium bond tier.** An opt-in tier lets an agent post its trust bond in
  `$CVNT` for higher trust weight and staking yield, alongside — never
  replacing — the default USDC bond. Enrollment is a one-time Solana action; it
  must never introduce a Solana touch into a per-call hot path.
- **Governance.** The stake program exposes no governance surface today
  (it is denomination-isolated from settlement and reads no settlement state).
  A future governance surface would let locked `$CVNT` steer protocol
  parameters.

Until those land, the honest claim is narrow: `$CVNT` captures value through
buy-and-lock real yield over a fee base that multichain usage enlarges, and the
quarantine guard guarantees the token never leaks onto another chain in the
process.
