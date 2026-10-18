# Covenant Compute for Python

Buy bounded inference with Solana USDC through the Covenant x402 gateway. The
client verifies the buyer job, operator receipt, exact output bytes, request and
quote binding, and the confirmed payment transaction before returning a completed
result. The Python package supports Microsoft Agent Framework through an optional
adapter; this is an integration, not a Microsoft affiliation or endorsement.

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
        messages=[{"role": "user", "content": "Summarize this support ticket."}],
        max_tokens=256, temperature=0.2,
    )
```

Use the same request UUID, parameters, wallet, gateway and journal when resuming.
A pending response is not a completed inference. Unknown outcomes retain their
authorization and budget reservation. The client never automatically changes or
re-signs an uncertain payment, even after its quote or blockhash expires. Restore
or reconcile the original journal before retrying. Deleting the journal resets
the local ceiling and destroys recovery information.

The SQLite schema and scope match `@covenant/compute-x402-client` v0.2.0. Node and
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

The official pinned x402 Python SVM scheme constructs an unsigned transaction
using a capture signer. Covenant checks its full compiled message before making
the explicit 20,000 to 60,000 compute-unit adjustment needed for the request memo.
It then signs with the supplied solders keypair or an adapter exposing `address`
and `sign_transaction(VersionedTransaction)`. That adapter must return the same
message, the buyer signature, and an empty facilitator signature. The facilitator
pays SOL fees; the buyer pays only the approved USDC transfer. Legacy 20,000-unit
authorizations already in a shared Node journal are reused byte for byte.

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
