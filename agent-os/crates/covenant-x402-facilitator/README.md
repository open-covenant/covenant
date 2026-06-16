# covenant-x402-facilitator

The verifier side of x402-over-ER. Counterpart to `covenant-x402`'s
`EphemeralSigner`: where the signer pays by running `consume_credits` in a
MagicBlock ephemeral rollup, this verifies that proof and gates a paid endpoint.

- `GET /paid`, no `x-payment` → **402** + an x402 challenge array carrying a fresh
  single-use `nonce` and the `amountCredits` price (shaped as `covenant-x402`'s
  `PaymentRequirements`, so its `Client` parses it directly).
- `GET /paid` with `x-payment` → decode the envelope, fetch the consume on the ER
  (`getTransaction`), and check: it's a `consume_credits` to the settlement program,
  `amount >= price`, `receipt_hash == sha256(nonce)`, and the nonce was issued and
  not yet consumed (anti-replay). Pass → **200** + content.

The credit account is read from the on-chain transaction, not the client envelope,
so a spoofed field can't lie about what was metered. It speaks JSON-RPC and base58,
so it needs neither solana-sdk nor the on-chain ER SDK (zero lock impact).

## Run

```bash
cargo run -p covenant-x402-facilitator           # PROGRAM, ER, PRICE, PORT via env
```

## Tests + live verification

Unit + wiremock tests cover the verifier (good payment, nonce mismatch, underpay,
receipt-not-bound-to-nonce, wrong-program). The full loop is **live-verified on
devnet**: the real `covenant-x402` `Client` + `EphemeralSigner` against a running
facilitator and the EU ER validator —

```bash
# 1) start the facilitator (PRICE=2)
PRICE=2 ER=https://devnet-eu.magicblock.app cargo run -p covenant-x402-facilitator
# 2) delegate the credit account
node ../../programs/settlement-ephemeral/spike/er-session.mjs delegate
# 3) drive the whole loop in Rust
cargo run -p covenant-x402-facilitator --example live_loop
```

Verified run: `GET /paid → 402 → ER consume → 200`, credit balance dropped by exactly
the price, reconciled to L1 on undelegate.
