# Spend Authorization

The daemon can act as the spending policy for an external agent wallet. A
wallet that holds its own keys (an OrbWallet, for example) asks the daemon
to approve a spend before it signs. The daemon checks the caller's
capability, a per-call cap, and the payer's budget, records the verdict in
the audit chain, and answers approve or deny. No funds move and no
settlement receipt is written. It is a decision, not a payment. Settlement
accounting, the budget debit and receipt after a payment lands, is the
separate outbound path documented with the x402 surface.

This is a daemon capability, not a wallet-specific one. Any wallet that can
make an authenticated HTTP call before it signs can use it.

## Enable it

Off by default. The operator opts in at boot:

- Set `COVENANT_SPEND_AUTHZ_ENABLED=1` in the daemon's environment.
- Grant the calling identity the capability:
  `covenant capabilities grant wallet.spend.authorize`.

The grant is the same operation over HTTP, which is what an integration
running against the gateway will use:

```bash
curl -sS -X POST http://127.0.0.1:8421/capabilities/grant \
  -H "Authorization: Bearer $COVENANT_TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"action":"wallet.spend.authorize"}'
```

With the flag unset the endpoint returns a "not configured" error; with it
set but the capability missing it returns an error naming the missing
capability. Neither state ever approves a spend.

## Endpoint

```
POST /spend/authorize
Authorization: Bearer <operator-or-peer-token>
Content-Type: application/json
```

Every gateway route except `/health` and `/version` requires the bearer
token; spend authorization is no exception. The gateway listens on
`127.0.0.1:8421` by default; set `COVENANT_HTTP_PORT` to change it. The
token is minted at `$COVENANT_HOME/peers/operator.token` on first start.

### Request

```json
{
  "provider": "orbserv",
  "network": "eip155:8453",
  "asset": "0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913",
  "amount": "80000",
  "per_call_cap": "100000",
  "credits": 8,
  "destination": "0xPayee"
}
```

| Field | Type | Meaning |
|---|---|---|
| `provider` | string | Free-form provider tag, recorded on the audit row. |
| `network` | string | CAIP-2 network the wallet intends to settle on (e.g. `eip155:8453` for Base, `solana:<genesis>` for Solana). |
| `asset` | string | Token contract (EVM) or mint (Solana) the spend is denominated in. |
| `amount` | string | Atomic amount as a decimal string. A string, not a number, so u128 values above JSON's 53-bit integer ceiling survive the wire. |
| `per_call_cap` | string | Maximum atomic amount one spend may request, as a decimal string. The bound the caller is enforcing for this call. |
| `credits` | number | USD-pegged budget the spend would consume. Derive it from `amount` the same way the x402 path derives the credits it debits. |
| `destination` | string, optional | Pay-to address, recorded on the audit row for triage. |

### Response

```json
{ "kind": "spend_authorized", "approved": true, "decision_id": "f47ac10b-58cc-4372-a567-0e02b2c3d479" }
```

On a deny:

```json
{ "kind": "spend_authorized", "approved": false, "decision_id": "9b1c0a7e-2f3d-4c5a-8e6f-0a1b2c3d4e5f", "reason": "amount 100001 exceeds the per-call cap 100000" }
```

| Field | Type | Meaning |
|---|---|---|
| `approved` | bool | The verdict. Sign only when `true`. |
| `decision_id` | uuid | Minted on every call (approve and deny). Keep it: a later settlement receipt can join back to the authorization that allowed the spend. |
| `reason` | string, optional | Present only on a deny. Operator-readable, safe to surface to the user. |

A policy deny is a `spend_authorized` response with `approved: false`, not
an HTTP error. Reserve error handling for transport and configuration
problems (missing capability, surface not enabled, malformed body), which
come back as `{ "error": "<message>" }`.

## Decision rules

A spend is approved only if these hold. Otherwise it is denied with the
first failing reason.

1. The caller holds `wallet.spend.authorize`.
2. `amount` parses as a decimal u128 and is `<= per_call_cap`.
3. The payer's budget would not be exceeded by `credits`. A payer with no
   configured budget bucket has no cumulative ceiling, so this check
   applies only once a budget is set; the per-call cap and the capability
   always apply. The check reads the budget and never debits.

The `network` and `asset` are recorded on the audit row. Binding the
allowed chains, assets, and per-call cap into the granted capability per
subject, instead of taking them from the request, is the planned next
step. Today the authenticated caller supplies the per-call bound, the same
model the x402 path uses. A real budget-subsystem failure denies,
fail-closed.

## Integration flow (wallet side)

1. Wallet receives a spend intent (an `x402` 402 challenge, or a direct
   transfer it is about to make).
2. Before signing, `POST /spend/authorize` with the spend's `network`,
   `asset`, `amount`, the `per_call_cap` you enforce, and the `credits`
   it costs.
3. On `approved: true`, sign and submit. On `approved: false`, abort and
   surface `reason`.
4. Keep `decision_id` with the transaction.
5. Once the payment lands on-chain, `POST /spend/settle` with that
   `decision_id` and the settled facts (see below). This is optional but
   it is what closes the loop: it records the receipt and joins the
   payment back to its authorization in the audit chain.

Set the wallet's own spending policy to mirror these bounds as a hard
floor. The daemon is the authority; the wallet policy is a backstop so a
spend can never exceed the bound even if a call skips the pre-flight.

## Settling a spend

After the wallet pays, report it so the daemon records the receipt and the
budget debit and links them back to the authorization. This moves no funds;
the wallet already paid with its own keys.

```
POST /spend/settle
Authorization: Bearer <operator-or-peer-token>
```

Requires the capability `wallet.spend.settle` (grant it the same way as
`wallet.spend.authorize`).

```json
{
  "decision_id": "f47ac10b-58cc-4372-a567-0e02b2c3d479",
  "provider": "orbserv",
  "network": "eip155:8453",
  "asset": "0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913",
  "amount": "80000",
  "credits": 8,
  "tx_sig": "0x..."
}
```

| Field | Type | Meaning |
|---|---|---|
| `decision_id` | uuid | The id from the `/spend/authorize` response this payment acted on. |
| `amount` | string | Atomic amount actually settled, decimal string. |
| `credits` | number | USD-pegged budget the spend consumed; debited from the payer's bucket when one is configured. |
| `tx_sig` | string, optional | On-chain transaction signature or hash, recorded on the receipt. |

Response:

```json
{ "kind": "spend_settled", "receipt_id": "821be8f3-cfa2-438a-aeae-90dac60c5352", "decision_id": "f47ac10b-58cc-4372-a567-0e02b2c3d479" }
```

`receipt_id` joins the budget debit, the settlement receipt, and the
`spend_settled` audit row. The `decision_id` is recorded for correlation;
this path does not yet verify it names a prior approved authorization, so
treat the join as accounting, not enforcement.

Settlement is idempotent on `decision_id`. Retry it freely: if the original
response was lost, or it failed after the debit landed, a repeat returns the
**same** `receipt_id` without debiting the budget again or writing a second
`spend_settled` row. One on-chain payment yields exactly one debit and one
row, so a client can safely retry (the reference OrbWallet client retries 3×
automatically and exposes `retryFailedSettlement(decisionId)`).

## Audit

Every decision writes one `spend_authorization_decided` row to the audit
chain, on both approve and deny, carrying `provider`, `network`, `asset`,
`amount`, `credits`, `destination`, `approved`, `reason`, and
`decision_id`. A settlement adds one `spend_settled` row carrying the same
`decision_id` plus the `receipt_id` and `tx_sig`, so the authorization and
the payment that acted on it read back as a linked pair. The chain is a
verifiable record of what each wallet was and was not allowed to spend, and
of what it then settled. Read it with `covenant audit recent` or
`GET /audit/recent`, and verify chain integrity with `GET /audit/verify`.

## Example

```bash
curl -sS -X POST http://127.0.0.1:8421/spend/authorize \
  -H "Authorization: Bearer $COVENANT_TOKEN" \
  -H "Content-Type: application/json" \
  -d '{
    "provider": "orbserv",
    "network": "eip155:8453",
    "asset": "0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913",
    "amount": "80000",
    "per_call_cap": "100000",
    "credits": 8,
    "destination": "0xPayee"
  }'
```
