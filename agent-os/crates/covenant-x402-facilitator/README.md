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

## Full daemon path (live, reproducible)

The end-to-end daemon integration is covered by an `#[ignore]`d test in `covenantd`,
`pay_x402_settles_through_the_er_signer_live`: it drives a real `PayX402` op through
`op_respond` → `X402Config::signer_for` → the ER signer sidecar → a `consume_credits`
in the ER → this facilitator → `200`, and asserts a settlement receipt was recorded.
Run it (delegate the credit account first via `er-session.mjs delegate`):

```bash
ER_SIGNER_BIN=target/debug/covenant-x402-er-signer \
FACILITATOR_BIN=target/debug/covenant-x402-facilitator \
ER_KEYPAIR="$HOME/.config/solana/id.json" \
cargo test -p covenantd --lib pay_x402_settles_through_the_er_signer_live -- --ignored --nocapture
```

Verified green on devnet: the daemon spawned the sidecar, the ER consume settled, the
facilitator returned 200, and a receipt was recorded.

## Deployed (Render)

Live devnet instance: **https://covenant-x402-er-facilitator.onrender.com** (Render
web service `covenant-x402-er-facilitator`, Frankfurt, built from
`deploy/Dockerfile.facilitator` and declared in the root `render.yaml` blueprint).
`/health` returns 200; `/paid` returns the x402 challenge. Verify a real payment
against the deployed URL with `spike/remote-check.mjs` (delegate the credit account
first):

```bash
node er-session.mjs delegate
URL=https://covenant-x402-er-facilitator.onrender.com/paid node remote-check.mjs
node er-session.mjs undelegate
```

Verified live against the deployed service: `GET /paid → 402 → consume_credits in
the ER → 200` with the settling signature returned. The service tracks
`feat/magicblock-er` until #93 merges; repoint it to `main` after (the blueprint
matches it by name). Devnet config; mainnet is gated on MagicBlock's validator
answer.
