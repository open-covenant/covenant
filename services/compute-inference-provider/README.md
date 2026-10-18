# Bounded inference provider bridge

A Node 24 service between the existing Rust `openai-compat` operator and UsePod.
It binds only `127.0.0.1:4044`. It serves one configured text model, non-streaming,
and does not relay tools, embeddings, images or arbitrary API paths. It has no
third-party runtime dependencies and is disabled by default.

The upstream token is placed in UsePod's required URL path internally. The Rust
operator receives only a local URL and a separate local bearer secret. The bridge
never logs upstream URLs, headers, payloads, credentials or exception messages.
Provider errors return fixed reason codes. A private diagnostic row records only HTTP status, response SHA-256 and byte length, model hash/match, choice count, fixed finish/content categories and integer usage. Invalid response bodies and provider-supplied strings are never retained in diagnostics. Successful OpenAI response bytes are retained
and returned intact after validation; a response containing the upstream secret
is refused. Redirects are refused.

## Configuration

Set `COVENANT_INFERENCE_PROVIDER_ENABLED=true` only after the model and caps are
qualified. All of these variables are mandatory when enabled:

| Variable | Meaning |
|---|---|
| `USEPOD_TOKEN` or `USEPOD_TOKEN_FILE` | Upstream token; `_FILE` names an absolute plain-text secret file, not JSON. Set exactly one. |
| `COVENANT_INFERENCE_PROVIDER_BEARER_TOKEN` or `_BEARER_TOKEN_FILE` | Independent secret for local paid POSTs; same plain-file convention. |
| `COVENANT_INFERENCE_PROVIDER_MODEL` | Exact accepted request and response model ID. |
| `COVENANT_INFERENCE_PROVIDER_DB_PATH` | Absolute SQLite path on the persistent volume. Never use a temporary filesystem in production. |
| `COVENANT_INFERENCE_PROVIDER_LIFETIME_BUDGET_MICRO_USDC` | Positive lifetime ceiling; includes unresolved reservations. |
| `COVENANT_INFERENCE_PROVIDER_DAILY_BUDGET_MICRO_USDC` | Positive rolling 24-hour ceiling, at most the lifetime ceiling. |
| `COVENANT_INFERENCE_PROVIDER_MAX_PRICE_INPUT_MICRO_USDC_PER_M` | Mandatory maximum input price per million tokens, in USDC microunits. |
| `COVENANT_INFERENCE_PROVIDER_MAX_PRICE_OUTPUT_MICRO_USDC_PER_M` | Mandatory maximum output price per million tokens, in USDC microunits. |
| `COVENANT_INFERENCE_PROVIDER_MAX_INPUT_BYTES` | Maximum encoded message-array bytes, up to 65,536. |
| `COVENANT_INFERENCE_PROVIDER_MAX_INPUT_TOKENS` | Conservative full input-token allowance, up to 2,000,000. Set to at least the qualified model's maximum context input capacity. |
| `COVENANT_INFERENCE_PROVIDER_MAX_OUTPUT_TOKENS` | Maximum requested completion tokens, up to 32,768. Injected when omitted by the caller. |

Optional: `COVENANT_INFERENCE_PROVIDER_PORT` (default `4044`) and
`COVENANT_INFERENCE_PROVIDER_UPSTREAM_TIMEOUT_MS` (default `60000`, maximum `120000`).
Malformed policy refuses startup without printing secret-bearing input.
Concurrency is fixed at one across all processes sharing the SQLite file. A bounded
startup retry handles only SQLite journal-mode contention; it cannot retry a paid
request or a financial transition.

The input-byte limit is not a tokenizer. The full configured input-token allowance
is reserved for every call, including benchmark probes. Qualification must confirm
that this allowance bounds the model's context input capacity, that UsePod honors
both price headers, and that `max_tokens` bounds billable completion tokens. Local
code cannot enforce an external provider's billing implementation. A returned usage
count beyond either allowance is treated as unresolved and blocks further spending.

## Canonical model identity

The configured model must match the provider's response exactly. Qualification of a
dated catalog identifier returned a model hash matching `claude-haiku-4.5`; that
mismatch was refused and its full reservation retained. Use `claude-haiku-4.5` as the
explicit request and advertised model when qualifying that provider route. The
response name does not prove a dated model revision. No alias acceptance, automatic
fallback, or rewriting of provider model identity is enabled.

## Existing Rust operator

```text
COVENANT_COMPUTE_NODE_EXECUTOR=openai-compat
COVENANT_COMPUTE_OPENAI_URL=http://127.0.0.1:4044/v1
COVENANT_COMPUTE_OPENAI_API_KEY=<same private bridge bearer>
COVENANT_COMPUTE_OPENAI_DEFAULT_MODEL=<qualified model>
```

Advertise only the configured model and `inference_call`; do not advertise
streaming, embedding, vision or tools. `GET /v1/models` is a local metadata read
and performs no upstream request. It returns the model only when enabled, with no
unresolved call and sufficient capacity for a maximum-sized permitted request.
`GET /health` returns `ready`, `blocked` or `disabled`, also without paid dispatch.
An unresolved outcome or exhausted budget therefore fails the Rust model health
check instead of leaving an unavailable inference model advertised.

## Durable budget and replay

1. Validate bearer, model, message shape and generation limits before paid work.
2. Within `BEGIN IMMEDIATE`, check concurrency and lifetime/rolling-day caps and
   commit the maximum reservation using SQLite WAL with synchronous `FULL`.
3. Dispatch once to the fixed UsePod HTTPS host with both price-ceiling headers.
4. Validate the genuine response and usage. Persist the response and a conservative
   cost upper bound, then return the response to the Rust operator.

Accounting uses `ceil(input_tokens × input_price / 1,000,000)` plus the equivalent
output term. This is a conservative ceiling, **not the actual UsePod charge**.
Completed costs remain in lifetime accounting permanently and in daily accounting
for 24 hours after completion. Uncertain reservations remain in both permanently.

The Rust executor sends an `Idempotency-Key` derived from buyer public key and job
UUID. It stays stable across worker restart/retry. A repeated key with the same
request replays the persisted response; a changed request conflicts. Separate jobs
with identical prompts have distinct keys and run independently. Benchmarks already
receive new job UUIDs, so each new probe also runs independently. A caller without
an explicit key gets a fresh request UUID and **no implicit content-based cache**;
such a caller must not retry a response lost after dispatch without reconciliation.

A timeout, error response, incomplete usage, excessive usage, process interruption,
or uncertain financial outcome retains the whole reservation and blocks all further
paid calls. There is no automatic retry and no automatic age-based release. Preserve
the database and reconcile against provider evidence; do not delete the journal or
retry the failed job with a fresh idempotency key. No HTTP endpoint can
release reservations or reset the budget. Changing the upstream token while retaining
the same journal is refused because the journal is bound to its hashed identity.

## Audited terminal loss

An operator may explicitly abandon an **unknown** outcome at its full reserved cost.
This does not claim a refund or a known actual debit. The original call stays unknown
and its full reservation stays in **both lifetime and daily accounting forever**.
An additive, immutable loss record removes only the global concurrency blockade.
The failed idempotency key can never dispatch again or return a fabricated response;
distinct new work is allowed only within the remaining configured ceilings.
In-flight and completed calls cannot use this operation. Repeating the exact same
audit is idempotent; changing the audit or financial fields is refused.

Save a private audit report identifying the failed request, observed provider evidence,
reason for abandonment and full-reservation loss decision. Use its SHA-256 as the audit
reference. Run locally with the service's existing environment and persistent journal:

```sh
node services/compute-inference-provider/reconcile.mjs --inspect key:FAILED_REQUEST_ID
node services/compute-inference-provider/reconcile.mjs --abandon-full-reservation \
  --id key:FAILED_REQUEST_ID --request-sha256 EXACT_REQUEST_SHA256 \
  --reserved-micro-usdc EXACT_FULL_RESERVATION --audit-sha256 AUDIT_REPORT_SHA256
```

The inspect command excludes response bodies and secrets. Reconciliation checks the
exact stored request digest and reservation under `BEGIN IMMEDIATE`, persists the audit
before allowing subsequent work, and performs no network request. There is no HTTP
reconciliation endpoint, automatic abandonment, refund, budget reset or retry.

## Run and test

```sh
node services/compute-inference-provider/server.mjs
node --test services/compute-inference-provider/test.mjs
```

Tests use local fixture HTTP and a test-only transport injection. No real provider
request is made. They cover pre-dispatch durability, private authorization, malformed
requests, budget rejection, cross-instance concurrency, restart replay, fresh
identical jobs, probes, unknown outcomes across day boundaries, strict response
validation and secret-redacted errors.

Provider reference: [UsePod spend controls](https://docs.usepod.ai/using/spend-controls/)
and [inference proxy](https://docs.usepod.ai/api/proxy/). The documented ceilings are
`X-Pod-Max-Price-Input` and `X-Pod-Max-Price-Output`, USDC microunits per million tokens.
