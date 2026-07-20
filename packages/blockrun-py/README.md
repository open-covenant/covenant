# covenant-blockrun

A trust receipt for every [BlockRun](https://blockrun.ai) call, for Python.

BlockRun settles your agent's paid calls over x402. It proves the money moved. It
does not bind that payment to what you asked for, which model actually served the
request, or what came back. This package wraps your BlockRun client's transport
and emits a **Covenant receipt** for each call — the same receipt the Covenant
daemon and `@covenant-org/blockrun` produce, so anyone can check it.

It never changes how the call is made or how the money moves.

## Install

```bash
pip install covenant-blockrun
```

## Use

BlockRun's SDK signs x402 payments inside a custom httpx transport. Wrap it:

```python
import httpx
from covenant_blockrun import ReceiptTransport

def on_receipt(receipt):
    # plain data — persist it, log it, post it to your audit trail
    print(receipt.verdict, receipt.model_served, receipt.payment.tx)

transport = ReceiptTransport(httpx.HTTPTransport(), on_receipt=on_receipt)
http_client = httpx.Client(transport=transport)

# hand http_client to your BlockRun / OpenAI-compatible client
```

The transport tees the response stream, so `on_receipt` fires once the response
body has been consumed and its stream closed (httpx does this for you on a normal
request). The caller's body is never touched. A call is receipted only on a 2xx
response. Pass `on_error=...` to observe any receipt-build failure; it never
affects the call.

Each completed call yields a `CallReceipt`. `receipt.verdict` is the point:
`delivered` when you got the model you asked for, `substituted` when the router
swapped it, `unverified` when no served model was reported — turning "cheapest
capable model" from a claim into something checkable.

## Verify a receipt

Offline, no network, no payment:

```python
from covenant_blockrun import verify_receipt

result = verify_receipt(receipt)
# {"valid": ..., "digest": ..., "verdictConsistent": ..., "expectedVerdict": ...}
```

`digest()` and the hashes are RFC 8785 (JCS) SHA-256, byte-identical to the Rust
`covenant-blockrun` crate and `@covenant-org/blockrun` — a receipt built here
verifies in the daemon and the TypeScript SDK, and vice versa.

Apache-2.0.
