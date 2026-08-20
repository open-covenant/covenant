# Covenant Compute control plane

This binary serves the authenticated desktop API and uses SQLite as the
authority for job ownership, idempotency, immutable quotes, and spend
reservations.

Required configuration:

```text
COVENANT_COMPUTE_DATABASE_PATH=$HOME/.local/share/covenant/compute.sqlite3
COVENANT_COMPUTE_PROVIDER=vast
COVENANT_COMPUTE_BETA_TOKENS_JSON=[{"owner":"beta-user","token":"replace-with-a-random-token","spend_cap_usdc_micros":5000000}]
COVENANT_VAST_API_KEY_FILE=$HOME/.config/covenant/vast-api-key
```

`COVENANT_COMPUTE_BIND` defaults to `127.0.0.1:8787`. The process refuses an
in-memory database, missing beta credentials, an unknown provider, or missing
Vast credentials.

Run exactly one control-plane process for a database and provider namespace.
Launch and cancellation coordination is process-local; multi-replica leader
election and distributed allocation locking are not implemented in this alpha.

All `/v1` routes require bearer authentication. Configured bearer tokens are
hashed in memory and are not written to SQLite or logs. Credential-bearing
workspace URLs are returned only by the single-job response that obtained them;
they are not stored or returned by job listings.

During beta, receipt usage is derived from the provider's immutable offer plus
the control plane's durable runtime boundary. It is spend-authority evidence,
not independent metering or on-chain settlement. The quoted maximum is a hard
upper bound on beta-account usage and future escrow settlement, not on the Vast
invoice. Vast does not expose a per-instance billing deadline. The control plane
requests deletion at the selected duration and retries failures, so operator
cost can continue until Vast confirms deletion. Solana settlement must replace
this evidence boundary before receipts are described as on-chain payment proof.

The Vast adapter requires returned offer evidence for host verification,
reliability, availability, direct-port capacity, architecture, and CUDA 12.4
compatibility before allocation. It prices the configured disk allocation and
admits only offers with zero per-byte upload and download charges; otherwise a
token holder could create costs outside the time-based allowance. The API's
`cuda_major` value is the major component of Vast's returned maximum host CUDA
compatibility, not a runtime probe. Jupyter readiness and the exact port mapping
can only be checked after the provider creates the instance.
