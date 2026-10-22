# Covenant Compute for Python

Buy bounded inference with Solana USDC through the Covenant x402 gateway. The
client verifies the buyer job, operator receipt, exact output bytes, request and
quote binding, and the confirmed payment transaction before returning a completed
result. The Python package supports Microsoft Agent Framework through an optional
adapter; this is an integration, not a Microsoft affiliation or endorsement.

Version 0.2.0 added model tariffs. Version 0.3.0 supports quotes with a
32-character payment memo, which earlier versions refuse before signing.
This source also includes candidate batch support; its deployment, funded
acceptance, and package publication require separate release evidence.

Python 3.11 or later is required. Install the immutable release wheel with its
published SHA256. For framework tools, use the `agent-framework` extra. The `demo`
extra also installs Microsoft's OpenAI client adapter for a separately metered
planner. The package does not bundle a model, wallet, or funded balance.

```python
from solders.keypair import Keypair
from covenant_compute_x402 import CovenantX402Client

# Load secrets privately; never commit them to source or an agent prompt.
async with CovenantX402Client(
    gateway_url=gateway_url, rpc_url=rpc_url,
    pay_to=merchant_key, gateway_buyer=buyer_key, operators=operator_keys,
    wallet=keypair, journal_path=private_journal_path,
    max_payment_micro_usdc=50_000, max_total_micro_usdc=500_000,
) as client:
    result = await client.purchase(
        request_id=stable_purchase_uuid,
        model=approved_model,
        pricing="model_tariff_v1",  # omit to retain legacy fixed-price behavior
        messages=[{"role": "user", "content": "Summarize this support ticket."}],
        max_tokens=256, temperature=0.2,
    )
```

Use the same request UUID, parameters, wallet, gateway and journal when resuming.
Set `pricing="model_tariff_v1"` to opt in; omission preserves legacy fixed-price
requests and signed results. Quote amounts may vary by model and requested `max_tokens` output cap. A
configured tariff can combine a model's base price with that cap. The payment
limit is a ceiling, not a fixed charge: `amountMicroUsdc` reports the exact
accepted upfront quote. This does not meter or refund tokens actually generated.
Tariff changes affect new quotes; resuming preserves the original quote, amount,
model, output cap and signed payment. Changing the model or cap requires a new
purchase UUID; changing the pricing mode also requires a new UUID. An opted-in
result must carry the same native tariff snapshot in its signed input and routing
requirement. The client checks its model, revision, validity at job issue time,
requested cap and exact rounded native job price. Missing tariff evidence is
refused. The x402 customer payment is checked separately from the operator price.

## Batch inference

`purchase_batch` submits 1–2 independent text chat requests in their original
order to `/v1/inference/batches`. The source candidate supports a mix of legacy
fixed-price and `model_tariff_v1` requests. Deployment and funded acceptance must
be established separately before using this endpoint in production.

```python
result = await client.purchase_batch(
    request_id=stable_batch_uuid,
    version=1,
    requests=[
        {"model": approved_model, "pricing": "model_tariff_v1",
         "messages": [{"role": "user", "content": prompt}], "max_tokens": 256}
        for prompt in approved_prompts
    ],
)
if result["status"] == "completed":
    drafts = result["texts"]  # same order as approved_prompts
```

There is one upfront USDC payment and one cumulative budget reservation for the
whole batch. The accepted quote includes a frozen `batchPricing` manifest with
the batch ID, ordered job IDs, model, output cap, pricing mode and retail amount
of every child. The client checks those fields against the request and aggregate
payment before signing. Native operator prices can differ from the retail price.

A completed result requires every child’s trusted buyer signature, operator
receipt, exact output bytes, original input and native tariff, plus an ordered
RFC6962 inclusion proof. The root binds the full child response; it does not
establish answer correctness. The client separately confirms the aggregate
payment transaction. Results use `texts` and `result`; single purchases continue
to use `text` and `result`.

Resume with the same UUID, ordered requests, wallet, gateway and private journal.
Pending responses, partial failures and unknown outcomes retain the original
authorization and full reservation. No replacement payment or partial rebuy is
automatic. A missing or failed child never becomes a completed batch. The same
journal scope shares the existing ceiling across single requests, batches, Node
and Python; creating a separate batch journal would lose that shared ceiling.
Offline completed replay revalidates every proof against the current trust pins.
The proof format supports 32 children; this candidate limits fresh purchases to
two so sequential execution fits the existing native job deadline. An already
signed larger batch remains recoverable under its original journal identity;
this does not authorize a new larger batch or replacement payment.
Each child retains the configured `max_input_bytes` limit, and the entire
canonical request is limited to 128,000 bytes. Streaming, tools, nested batches
and unknown child fields are rejected before reservation or network access.

A pending response is not a completed inference. Unknown outcomes retain their
authorization and budget reservation. The client never automatically changes or
re-signs an uncertain payment, even after its quote or blockhash expires. Restore
or reconcile the original journal before retrying. Deleting the journal resets
the local ceiling and destroys recovery information.

The SQLite schema and scope match `@covenant/compute-x402-client` v0.4.0 and retain
compatibility with v0.2.0 legacy purchases. Node and
Python processes using the same wallet, exact gateway origin and journal share a
cumulative ceiling and exclusive five-minute purchase claims. Preserve the
existing ceiling when changing clients. A completed journal replay does no RPC,
signing, planner or gateway work. The ceiling is cumulative, not a rolling daily
limit. Separate journals cannot enforce a shared ceiling.

The application fixes wallet, trusted recipient/buyer/operator keys, model,
generation limits, prompt policy and request UUID. Only the prompt belongs in the
tool schema. Framework retries reuse the fixed UUID; a changed request is refused.
Signed receipts bind the named operator's output to the requested job; they do
not establish answer correctness. Payment is checked separately against the
confirmed Solana transaction. Review generated drafts before any communication.

## Microsoft Agent Framework

Install the wheel with the `agent-framework` extra, then register the optional
adapter with Microsoft's official `Agent`. The application supplies an already
configured client and its separately budgeted planner. This function runs inside
the client context shown above; persist `purchase_id` before calling it.

```python
from agent_framework import Agent
from covenant_compute_x402.agent_framework import covenant_inference_tool

async def review_ticket(client, planner, purchase_id, approved_model, ticket):
    tool = covenant_inference_tool(
        client=client, request_id=purchase_id, model=approved_model,
        max_tokens=512, temperature=0.2,
        pricing="model_tariff_v1",  # fixed by the application, outside the tool schema
        system_prompt="Draft for human review. Do not claim you changed an account.",
    )
    agent = Agent(
        client=planner, name="Support reviewer", tools=[tool],
        instructions="Use the configured inference tool when helpful. Review its draft.",
        default_options={"tool_choice": "auto", "max_tokens": 512},
    )
    return await agent.run(ticket)
```

For a saved workflow with explicit plan, purchase, replay and finish phases, use
`covenant_compute_x402.workflow_demo.run_demo`. It uses Microsoft's official
`WorkflowBuilder` and `FileCheckpointStorage`; its process lock supports Linux
and macOS. Keep that state directory private and never import checkpoints from
untrusted sources. The optional `demo` extra supplies
`agent_framework.openai.OpenAIChatCompletionClient`; configure its underlying
client with the planner endpoint, credentials and a separate durable budget.
The planner is not included in the Covenant inference price.
The optional `pricing` setting is also supported by `run_demo` and saved in its
workflow identity.

If a quote also offers payment on other networks, such as USDC on Base, the SDK
pays the Solana offer and ignores the others.
The SDK builds an unsigned transaction with the official x402 Python package's
Solana scheme (pinned) and a capture signer, so it can inspect the transaction
before anything is signed. Short-memo quotes are signed exactly as built, at the
official client's 20,000 compute units (13,180 used in a read-only mainnet
simulation); for a quote with the earlier 118-byte memo, which needs 43,642
units, only the limit is raised to 60,000. It signs with the supplied `solders`
keypair or an adapter exposing `address` and
`sign_transaction(VersionedTransaction)`, which must return the transaction with
its message unchanged, the buyer's signature added and the fee-payer signature
left empty for Covenant's verifier. Covenant pays the Solana fees; the buyer pays
only the USDC amount set by the quote. Both SDKs record purchases in the journal
file set by `journal_path` (`journalPath` in JavaScript). With a shared journal, a
payment the JavaScript SDK already signed for the same request is reused byte for
byte at either compute-unit limit.

New payments require the configured RPC to report the Solana mainnet genesis,
the canonical six-decimal USDC mint, and a sufficient associated token balance.
Transfers, fee payer, priority fee, account permissions, memo, token program and
amount are checked before signing. HTTP redirects are refused, JSON rejects
duplicate keys and invalid UTF-8, and response bodies are bounded.

Local qualification:

```sh
python -m pip install '.[agent-framework,test]'
python -m pytest
python -m build
```

`requirements-lock.txt` pins the release qualification environment, including
optional demo and test dependencies. Tests use generated unfunded keys and
synthetic RPC responses. Live funded acceptance is recorded separately in the
release evidence, never inferred from test success.
