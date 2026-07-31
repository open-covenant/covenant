# Spend Authorization

> **Current status: advisory preflight, not signer enforcement.** The
> authenticated caller supplies the proposed network, asset, amount, per-call
> cap, credits, and destination. The returned decision is not bound to
> transaction bytes, and the signer does not require or consume it. Keep an
> independent wallet policy and do not treat `approved: true` as a W009-style
> signing authorization.

An external wallet can ask the daemon for an advisory spend decision before it
signs. The daemon checks that the caller holds the endpoint capability, compares
the caller-supplied amount with the caller-supplied cap, consults an optional
budget bucket, records the verdict in the audit chain, and answers approve or
deny. No funds move and no settlement receipt is written. Settlement accounting,
the budget debit, and the receipt after a payment lands are handled separately
by the `/spend/settle` report described below.

This is a daemon endpoint, not a wallet enforcement boundary. Any wallet that
can make an authenticated HTTP call can use it, skip it, or ignore its result.

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
| `per_call_cap` | string | Caller-supplied maximum atomic amount for this request. It is not taken from the granted capability in the current implementation. |
| `credits` | number | Caller-supplied budget units the spend would consume. The daemon does not independently derive this value from `amount`. |
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
| `approved` | bool | Advisory verdict. It is never sufficient by itself to sign; the wallet must still validate and authorize the final transaction. |
| `decision_id` | uuid | Minted on every call (approve and deny). Settlement accepts it only when the stored decision was approved for the same payer and spend facts. |
| `reason` | string, optional | Present only on a deny. Operator-readable, safe to surface to the user. |

A policy deny is a `spend_authorized` response with `approved: false`, not
an HTTP error. Reserve error handling for transport and configuration
problems (missing capability, surface not enabled, malformed body), which
come back as `{ "error": "<message>" }`.

## Decision rules

A spend is approved only if these hold. Otherwise it is denied with the
first failing reason.

1. The caller holds `wallet.spend.authorize`.
2. `amount` parses as a decimal u128 and is `<=` the caller-supplied
   `per_call_cap`.
3. The payer's budget would not be exceeded by the caller-supplied `credits`. A payer with no
   configured budget bucket has no cumulative ceiling, so this check
   applies only once a budget is set. Inside this endpoint, the caller-supplied
   per-call cap and endpoint capability always apply. The check reads the budget
   and never debits.

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
3. Independently validate the final transaction and apply the wallet's own
   policy. Sign only when both that policy and the advisory response allow it.
   On `approved: false`, abort and surface `reason`.
4. Keep `decision_id` with the transaction.
5. After independently confirming the payment on-chain, the caller may
   `POST /spend/settle` with that `decision_id` and the reported facts (see
   below). The daemon requires the stored decision to be approved for the same
   authenticated payer and exact provider, network, asset, amount, and credits.
   It does not inspect or confirm the transaction on chain.

The wallet remains the enforcement boundary. This endpoint records an advisory
decision and optional budget check; it does not prevent a wallet or caller from
bypassing it. The wallet must independently validate the final transaction and
enforce its own limits before signing.

## Settling a spend

After independently confirming the wallet paid, report it so the daemon records
a receipt and budget debit keyed by the stored authorization and caller-reported
transaction signature. This endpoint moves no funds and does not query the
chain.

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
| `decision_id` | uuid | ID returned by `/spend/authorize`. Settlement requires a stored approved decision bound to the same payer and spend facts. |
| `provider` | string | Must exactly match the stored authorization. |
| `network` | string | Must exactly match the stored authorization. |
| `asset` | string | Must exactly match the stored authorization. |
| `amount` | string | Caller-reported atomic amount, decimal string. It must exactly match the stored authorization and is not derived from the transaction. |
| `credits` | number | Caller-reported budget units. They must exactly match the stored authorization and are debited from the payer's bucket when one is configured. |
| `tx_sig` | string, optional | Caller-reported transaction signature or hash, bound to the retry claim and recorded without chain verification. |

Response:

```json
{ "kind": "spend_settled", "receipt_id": "821be8f3-cfa2-438a-aeae-90dac60c5352", "decision_id": "f47ac10b-58cc-4372-a567-0e02b2c3d479" }
```

`receipt_id` is deterministic over the payer and `decision_id`; it joins the
budget debit, settlement receipt, and `spend_settled` audit row. Before debiting,
the daemon persists a claim for that key with a SHA-256 commitment over the payer,
authorized destination, provider, network, asset, amount, credits, and reported
`tx_sig`. An exact retry can recover after a receipt or audit write failure
without debiting twice. Reusing the decision with changed facts fails closed.
Budget compaction retains the claim and a debit tombstone, so idempotency
survives compaction and restart. Those keys are retained indefinitely and make
the ledger grow with unique settlements; compaction bounds operator-visible
debit history, not the idempotency index.

The guarantee is process-local and depends on the JSONL writes that were
successfully persisted. The stores do not share one transaction, the JSONL
backend does not promise `fsync` power-loss durability, and multiple daemon
processes must not share the same files. Legacy authorization rows without a
payer and legacy partial receipts without a fact claim are refused for operator
reconciliation rather than adopted. The authorization audit row must remain
within retention until settlement and any retry complete; if it has been purged,
settlement fails closed.

Each settlement scans the retained audit and receipt logs while holding the
process-local settlement lock. A fully completed legacy settlement without a
claim can be replayed only while its matching audit row and receipt remain
available. Covenant does not backfill a claim for that row because an older
compaction may already have discarded its debit idempotency key.

## Audit

Every decision writes one `spend_authorization_decided` row to the audit
chain, on both approve and deny, carrying `provider`, `network`, `asset`,
`amount`, `credits`, `destination`, payer identity, `approved`, `reason`, and
`decision_id`. A settlement adds one `spend_settled` row carrying a
validated `decision_id` plus the `receipt_id` and caller-reported `tx_sig`. The
daemon checks and records the local accounting join to the stored approval; it does not
prove that the transaction exists, matches the report, or paid the authorized
destination on chain.
Read them with `covenant audit recent` or `GET /audit/recent`, and verify local
chain integrity with `GET /audit/verify`.

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
