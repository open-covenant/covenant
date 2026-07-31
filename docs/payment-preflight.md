# Covenant Payment Preflight

Covenant Payment Preflight is a deterministic policy check for a proposed
x402 payment. Version 1 is intentionally narrow: sponsored Solana USDC,
x402 v2 `exact`, an explicitly allowed HTTPS resource, a trusted funder and
fee payer, classic SPL Token, and a static v0 transaction profile.

The evaluator accepts two separate inputs:

- `PaymentIntentV1` describes the untrusted payment proposal and the request
  it would purchase.
- `PaymentPolicyV1` is trusted local configuration. It binds the proposal to
  exact network, mint, token program, funder, source token account, fee payer,
  resource route, recipient, destination token account, amount and compute
  limits.

`evaluate_preflight` returns a `PreflightReceiptV1` carrying content hashes for
the intent and policy. Hashes use JCS canonical JSON with domain separation:

```text
SHA256("covenant.payment-intent.v1\0" || JCS(intent))
SHA256("covenant.payment-policy.v1\0" || JCS(policy))
```

The checked-in contracts are:

- [`payment-intent-v1.schema.json`](./schemas/payment-intent-v1.schema.json)
- [`payment-policy-v1.schema.json`](./schemas/payment-policy-v1.schema.json)
- [`preflight-receipt-v1.schema.json`](./schemas/preflight-receipt-v1.schema.json)

The schemas define closed, versioned wire shapes. They deliberately admit
well-typed but disallowed input values so a deny receipt can carry the exact
proposal it rejected. The Rust evaluator owns the semantic policy checks below.

## Supported profile

The v1 evaluator allows a proposal only when all of these are true:

- x402 version is `2` and scheme is `exact`.
- The selected x402 requirement and HTTP request body carry lowercase SHA-256
  digests.
- The payment identifier is 16–128 alphanumeric, hyphen or underscore
  characters, and the Memo equals it.
- The request is HTTPS, has no credentials or fragment, and exactly matches a
  policy origin, method, path and query.
- Network and mint are the matching Solana mainnet/devnet USDC pair.
- Token program is classic SPL Token and decimals are six.
- Funder, source token account and fee payer match the policy.
- Recipient and destination token account match the selected route.
- Required and proposed transfer amounts are the same positive canonical
  `u64`, and do not exceed the route limit.
- Compute-unit limit and price are positive and within policy.
- The proposal is observed, unexpired and no longer-lived than policy allows.

Checks run in a fixed order and reason codes are stable. An unsupported or
malformed policy denies closed with `policy_invalid`.

`verify_preflight_receipt` deterministically replays the same evaluator against
the exact policy document and checks receipt self-consistency. It is not an
independent attestation: the v1 receipt is unsigned, and both the intent and
`evaluated_at_unix` are caller-supplied. The policy is not embedded in the
receipt, so a verifier must receive it separately and confirm its content hash.

## Security boundary

This is advisory preflight, not signing enforcement. Version 1 does not:

- derive or read Solana accounts;
- parse the upstream challenge or recompute its accepted-requirement hash;
- build, decode, simulate or sign a transaction;
- prove that the request bytes sent over HTTP match the supplied hashes;
- consume the decision at a signer;
- isolate a signing key;
- reserve a one-use authorization or prevent replay;
- submit a transaction or verify settlement and finality.

Every v1 receipt therefore serializes these exact values:

```json
{
  "mode": "advisory",
  "decision_consumed_by_signer": false,
  "signing_key_isolated": false,
  "durable_single_use": false
}
```

The Rust types reject `true` for any of those fields. A signer-bound result
requires a different contract after the final transaction is independently
decoded, matched to the intent, authorized once and signed.

## Verification

Run the focused default-feature suite from `agent-os`:

```bash
cargo test -p covenant-x402 --test preflight_contract --test preflight_policy
```

These tests pin the allow fixture, canonical hashes, closed wire shapes,
false-only enforcement boundary, deterministic denial codes, strict Solana
scope and the mutation matrix. They do not constitute a live-chain or wallet
witness.
