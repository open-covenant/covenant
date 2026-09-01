# Mizuki facilitator

Self-hosted x402 `exact` facilitator for Solana mainnet. The Mizuki runtime
calls it to verify and settle USDC payments; it co-signs each payment as fee
payer and broadcasts it.

It exists because no public facilitator accepts a payment signed by a wallet
that brackets the transfer with its own guard instructions. Phantom inserts
Lighthouse guards both before and after the `TransferChecked`, so the published
verifier — which matches instructions positionally — rejects every Phantom
payment. This service runs that verifier first and only re-examines a payment
it already rejected, against the same payment requirements. Duplicate
detection, settlement, reconciliation and confirmation stay with the library.

Retire it once [x402#3318](https://github.com/x402-foundation/x402/pull/3318)
ships and the upstream facilitator you use has picked it up: point
`MIZUKI_X402_FACILITATOR` back at that facilitator and delete this service.

## Boundaries

- Runs as a Render private service. It is not reachable from the internet, and
  it still requires a bearer token, because anything on the private network
  could otherwise spend its fee payer.
- The fee payer holds a small SOL float for network fees only — never user
  funds, never the refund treasury. It refuses to start when the configured
  key does not derive `MIZUKI_FACILITATOR_FEE_PAYER_PUBLIC_KEY`.
- It never chooses payment terms. Every payment is checked against the
  requirements the runtime supplies, so a caller cannot redirect a payment.

## Configuration

| Variable                                        | Meaning                                                                     |
| ----------------------------------------------- | --------------------------------------------------------------------------- |
| `MIZUKI_FACILITATOR_RPC_URL`                    | Solana mainnet RPC (HTTPS).                                                 |
| `MIZUKI_FACILITATOR_FEE_PAYER_PRIVATE_KEY_JSON` | Fee payer secret key: JSON array of 64 bytes, as `solana-keygen` writes it. |
| `MIZUKI_FACILITATOR_FEE_PAYER_PUBLIC_KEY`       | Base58 address the key must derive.                                         |
| `MIZUKI_FACILITATOR_TOKEN`                      | Bearer token the runtime presents.                                          |
| `MIZUKI_FACILITATOR_PORT`                       | Listen port; defaults to 8402.                                              |

## Endpoints

`GET /healthz` and `GET /readyz` are unauthenticated for the platform probe.
`GET /supported`, `POST /verify` and `POST /settle` require the bearer token
and follow the x402 facilitator protocol.
