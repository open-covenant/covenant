# Spend Authorization

The daemon can act as the spending policy for an external agent wallet. A
wallet that holds its own keys (for example an OrbWallet) asks the daemon
to approve a spend **before it signs**; the daemon checks the caller's
capability, a per-call cap, and the payer's budget, records the verdict in
the audit chain, and answers approve or deny. No funds move on this path
and no settlement receipt is written — it is a decision, not a payment.
Settlement accounting (the budget debit and receipt after a payment
actually lands) is the separate outbound path documented alongside the
x402 surface.

This is a daemon capability, not a wallet-specific one: any wallet that
can make an authenticated HTTP call before it signs can use it.

## Enable it

Off by default. The operator opts in at boot:

- Set `COVENANT_SPEND_AUTHZ_ENABLED=1` in the daemon's environment.
- Grant the calling identity the capability:
  `covenant capabilities grant wallet.spend.authorize`.

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
token; spend authorization is no exception.

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
{ "kind": "spend_authorized", "approved": false, "decision_id": "…", "reason": "amount 100001 exceeds the per-call cap 100000" }
```

| Field | Type | Meaning |
|---|---|---|
| `approved` | bool | The verdict. Sign only when `true`. |
| `decision_id` | uuid | Minted on every call (approve and deny). Keep it: a later settlement receipt can join back to the authorization that allowed the spend. |
| `reason` | string, optional | Present only on a deny. Operator-readable, safe to surface to the user. |

A policy deny is a `spend_authorized` response with `approved: false`, not
an HTTP error. Reserve error handling for transport and configuration
problems (missing capability, surface not enabled, malformed body), which
come back as `{ "error": "…" }`.

## Decision rules

A spend is approved only if all hold; otherwise it is denied with the
first failing reason. The check is **fail-closed**: any budget-subsystem
error denies rather than letting the spend through.

1. The caller holds `wallet.spend.authorize`.
2. `amount` parses as a decimal u128 and is `<= per_call_cap`.
3. `network` and `asset` match the request's own policy fields.
4. The payer's budget would not be exceeded by `credits` (this reads the
   budget; it does not debit).

The per-call cap is supplied by the authenticated caller. Per-subject
scoped caps — binding allowed chains, assets, and ceilings into the
granted capability itself rather than trusting the request — are the
planned next step; today the calling identity is trusted to pass the
bound, the same model the x402 path uses.

## Integration flow (wallet side)

1. Wallet receives a spend intent (an `x402` 402 challenge, or a direct
   transfer it is about to make).
2. Before signing, `POST /spend/authorize` with the spend's `network`,
   `asset`, `amount`, the `per_call_cap` you enforce, and the `credits`
   it costs.
3. On `approved: true`, sign and submit. On `approved: false`, abort and
   surface `reason`.
4. Keep `decision_id` with the transaction so the payment can later be
   correlated to its authorization.

Set the wallet's own spending policy to mirror these bounds as a hard
floor. The daemon is the authority; the wallet policy is a backstop so a
spend can never exceed the bound even if a call skips the pre-flight.

## Audit

Every decision writes one `spend_authorization_decided` row to the audit
chain — on approve and on deny — carrying `provider`, `network`, `asset`,
`amount`, `credits`, `destination`, `approved`, `reason`, and
`decision_id`. The chain is therefore a complete, verifiable record of
what each wallet was and was not permitted to spend, independent of
whether the wallet later settled. Read it with `covenant audit recent` or
`GET /audit/recent`, and verify chain integrity with `GET /audit/verify`.

## Example

```bash
curl -sS -X POST http://127.0.0.1:8787/spend/authorize \
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
