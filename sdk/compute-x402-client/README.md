# Covenant inference for AgentKit, LangChain and LangGraph

Buy inference from Covenant with Solana USDC using standard x402 V2. The package provides a Coinbase AgentKit action and an official LangChain tool usable in LangGraph agents. Both use the same payment journal, spending limits and signed-result verification. The configured Solana wallet signs the actual SPL-token transfer through `walletProvider.signTransaction`.

Version 0.1.0 completed funded AgentKit acceptance and is available as an immutable [GitHub release](https://github.com/open-covenant/covenant/releases/tag/compute-x402-client-v0.1.0). Version 0.2.0 added LangChain/LangGraph support. Version 0.3.0 added model tariffs. Version 0.4.0 supports quotes with a 32-character payment memo, which earlier versions refuse before signing. This branch also contains the unpublished batch candidate described below; the package version does not establish batch deployment or funded acceptance. Compatibility does not imply a partnership or endorsement.

## Install

Requires Node.js 24 or later (the durable journal uses Node's SQLite implementation).

Until a registry release is published, install the checked source package from
this release branch:

```sh
git clone --filter=blob:none --branch deploy/compute-coordinator \
  https://github.com/open-covenant/covenant.git covenant-x402
cd covenant-x402/sdk/compute-x402-client
npm ci --ignore-scripts
# Include the tests that pay through the real payment verifier (install its
# dependencies first with npm ci in services/compute-x402-facilitator):
COVENANT_X402_BRIDGE_PATH=$PWD/../../services/compute-x402-facilitator/bridge.mjs npm test
npm pack

# In the application that will use the client:
npm install /absolute/path/to/covenant-compute-x402-client-0.4.0.tgz
```

The packaged `demo.mjs` is also available at
`node_modules/@covenant/compute-x402-client/demo.mjs` after installation.

Direct dependencies and the complete dependency tree are locked. The Coinbase AgentKit dependency is `0.10.4`; official `@x402/core` and `@x402/svm` are `2.28.0`. AgentKit has a large dependency tree; its optional native bigint acceleration is not required.

The framework versions are `@langchain/core` 1.2.18, `@langchain/langgraph` 1.5.1 and `langchain` 1.5.18. Only the optional example/test dependencies need `@langchain/openai` 1.6.2 and `@langchain/langgraph-checkpoint-sqlite` 1.1.0. A planner's own inference requests and credentials are separate from Covenant purchases and require their own spending controls.

## Register with LangChain or LangGraph

```js
import { createAgent } from 'langchain';
import { CovenantX402Client } from '@covenant/compute-x402-client';
import { covenantInferenceTool } from '@covenant/compute-x402-client/langchain';

const client = new CovenantX402Client({
  gatewayUrl, rpcUrl, payTo, gatewayBuyer,
  operators: [verifiedInferenceOperatorPublicKey],
  walletProvider,
  journalPath: '/private/persistent/path/purchases.sqlite',
  maxPaymentMicroUsdc: 50_000,
  maxTotalMicroUsdc: 500_000,
});
const inference = covenantInferenceTool({
  client,
  requestId: savedPurchaseUuid, // application persists this before the first run
  model: enabledModel,
  pricing: 'model_tariff_v1', // optional application-owned opt-in
  maxTokens: 2048,
  temperature: 0.2,
  systemPrompt: 'Draft only. You have no account access and have taken no actions. Do not claim refunds, escalation or account changes.',
});
const agent = createAgent({ model: planner, tools: [inference], checkpointer });
try {
  const result = await agent.invoke({ messages: [
    { role: 'user', content: 'Use Covenant inference to draft a response to this example support ticket: ...' },
  ] }, { configurable: { thread_id: savedThreadId } });
  // A ToolMessage contains the verified answer/payment summary in content,
  // and the full accepted client response (including receipt) in artifact.
} finally {
  client.close();
}
```

The same tool works with `createReactAgent({ llm: planner, tools: [inference], checkpointSaver: checkpointer })` from `@langchain/langgraph/prebuilt`. Current LangChain applications can use `createAgent` above. Configure an appropriate persistent checkpointer in the application; `MemorySaver` alone does not survive process restart. A graph checkpoint and the payment journal serve different purposes and both must be retained.

The model sees only a strict `{ prompt: string }` schema. The application fixes the model, token cap, temperature, purchase UUID, wallet, gateway, trusted identities and spending ceilings. One tool instance authorizes one logical purchase. The purchase UUID must not come from the model's tool-call ID, a graph attempt ID, or a fresh UUID generated during retry. To authorize another purchase, the application explicitly persists another UUID and constructs another tool. Each purchase still shares the wallet's existing cumulative journal ceiling.

The optional application-controlled `systemPrompt` is sent as an exact system message before the tool's user prompt and included in the signed request/receipt binding. It is not a model-visible tool argument. Omitting it preserves the original user-only request. Changing or removing it under the same purchase UUID is refused; both messages count toward the client's UTF-8 input limit. Instructions help constrain the draft but do not guarantee factual accuracy or replace review.

Recreating the tool with the same UUID and journal resumes the same authorization. A different prompt or generation setting under that UUID is refused before network access. Concurrent recovery attempts do not sign twice: an active claim returns `purchase_in_progress`; after the owning process exits unexpectedly, its claim can remain locked for up to five minutes. Do not respond to a pending result or error by changing the purchase UUID. A completed cached result makes no network request.

Tool output is untrusted task data for the planner. A verified receipt authenticates the operator's answer and its binding to the request; it does not establish factual correctness or authorize execution of generated instructions. The tool never executes the answer. `ToolMessage.artifact` can contain the original prompt and response within signed evidence; apply the application's access and tracing policy to retained checkpoints.

The optional `runSupportDemo` export from `@covenant/compute-x402-client/langgraph-demo` demonstrates a persistent support-ticket workflow. Install its optional dependencies explicitly:

```sh
npm install @langchain/openai@1.6.2 @langchain/langgraph-checkpoint-sqlite@1.1.0
# If your installation disabled native dependency scripts:
npm rebuild better-sqlite3 --ignore-scripts=false
```

The application supplies the bounded client and planner. Run `plan`, `purchase`, `replay` and `finish` with the same private output directory, trusted purchase UUID and graph thread ID. `plan` asks the model to choose whether to call the tool and checkpoints before payment. `purchase` resumes that selection. `replay` re-executes the saved tool call against the original payment journal and requires the same verified result. `finish` is permitted only after a completed purchase. Pending purchases resume the original pre-tool checkpoint; they cannot be presented as a finished answer. Each phase can run in a new process. The example uses a labeled sample ticket and does not contact a customer or issue refunds.

## Register with AgentKit

```js
import { AgentKit } from '@coinbase/agentkit';
import { covenantInferenceActionProvider } from '@covenant/compute-x402-client/agentkit';

const provider = covenantInferenceActionProvider({
  gatewayUrl, rpcUrl, payTo, gatewayBuyer,
  operators: [verifiedInferenceOperatorPublicKey],
  journalPath: '/private/persistent/path/purchases.sqlite',
  maxPaymentMicroUsdc: 50_000, // launch ceiling: 0.05 USDC per purchase
  maxTotalMicroUsdc: 500_000,  // example: 0.50 USDC cumulative ceiling
});
const kit = await AgentKit.from({ walletProvider, actionProviders: [provider] });
const action = kit.getActions().find(a => a.name === 'covenant_inference');
const result = JSON.parse(await action.invoke({
  requestId: 'ae81b592-a97e-425b-a315-987bc2dfd782',
  model: enabledModel,
  pricing: 'model_tariff_v1', // omit to retain legacy fixed-price behavior
  messages: [{ role: 'user', content: 'Return a complete, self-contained HTML webpage for an interactive meeting cost calculator. Include CSS and JavaScript inline, no external resources. Return HTML only.' }],
  max_tokens: 2048,
  temperature: 0.2,
}));
provider.close(walletProvider);
```

`walletProvider` must expose AgentKit's Solana transaction-signing interface on mainnet. The demo uses `CovenantSolanaKeypairWalletProvider`, a subclass of Coinbase's `SolanaKeypairWalletProvider`. It disables only the upstream initialization analytics hook, whose unawaited HTTP error can terminate Node in AgentKit 0.10.4. All custody and signing behavior remains the original AgentKit implementation. If a quote also offers payment on other networks, such as USDC on Base, the SDK pays the Solana offer and ignores the others. For quotes with the 32-character memo, the wallet signs the transaction exactly as the official x402 client (`@x402/svm`) builds it, with the client's default limit of 20,000 compute units; such a payment used 13,180 units in a read-only mainnet simulation. Earlier gateway versions issue a 118-byte memo, which needs 43,642 units; for those quotes the SDK raises the compute-unit limit to 60,000 before the wallet signs and changes nothing else. Before the wallet signs, the SDK checks that the transaction carries no signatures yet, and after signing it rejects the result if the wallet has changed the transaction message.

Configure the gateway origin, receiving wallet, gateway Compute buyer and allowed operator public keys from the verified deployment. HTTPS is mandatory; an explicit `allowLocalhost` option exists only for local fixtures. A redirect is an error. The RPC genesis hash must be Solana mainnet before any quote or payment signature. Requests contain at most 32 text messages. `maxInputBytes` defaults to the launch limit of 4096 UTF-8 bytes of the normalized JSON message array, including JSON framing and escaping. A different deployed limit may be configured explicitly, up to 65536 bytes. Oversized input is refused before quoting or reserving money.

## Demo and evidence

Create a private configuration JSON file with the fields above and `keypairPath` pointing to a mode-0600 Solana CLI keypair JSON file (64 bytes). Set the funded wallet and budgets explicitly; there is no automatic funding. Put the action arguments, including a stable UUID `requestId`, in a separate request JSON file.

```sh
node demo.mjs /private/config.json /private/request.json /private/demo-output
```

The command invokes the **registered AgentKit action**, saves the exact verified response and public evidence, and produces a local `page.html` when the response is HTML. The preview adds a restrictive content-security policy; `response.txt` retains the unchanged signed output. Inspect the page before presenting it. Tool registration does not by itself imply autonomous planning by an LLM; the recorded demo must describe the orchestration it actually uses.

The client returns `completed` only after verifying:

- the exact request/quote memo, Solana mainnet USDC and configured recipient;
- the gateway buyer's signed job and its model, messages and generation parameters;
- the allowed operator's signed receipt, job hash, output bytes, status and offered price ceiling;
- the displayed answer against the signed output;
- the confirmed payment transaction from the configured RPC against the exact authorized transaction message.

Customer payment and operator receipt amounts are separate: the former is the gateway's USDC charge, the latter is the Compute job's operator price. An operator receipt does not independently prove model quality or the operator's hardware.

## Batch inference candidate

The source candidate adds `client.purchaseBatch()` at `/v1/inference/batches`. It is not a released or funded-accepted batch client. The existing `purchase()`, AgentKit action and LangChain/LangGraph tool retain their single-request behavior.

```js
const result = await client.purchaseBatch({
  requestId: durablePurchaseUuid,
  version: 1,
  requests: [
    {
      model: 'configured-model',
      messages: [{ role: 'user', content: 'Classify this first support ticket: ...' }],
      max_tokens: 128,
      pricing: 'model_tariff_v1',
    },
    {
      model: 'configured-model',
      messages: [{ role: 'user', content: 'Classify this second support ticket: ...' }],
      max_tokens: 256,
      pricing: 'model_tariff_v1',
    },
  ],
});
if (result.status === 'completed') {
  console.log(result.texts); // Request order; complete native evidence in result.result.
}
```

This first sequential-operator candidate admits 1–2 independent supported text-chat requests per new purchase. The proof format and recovery of already-signed historical authorizations retain the 32-child bound. Each child may opt into `model_tariff_v1`; omitting it retains that child's legacy fixed-price behavior. Nested batches and unsupported generation parameters are refused. The configured input-byte limit applies to each child's messages, with a 128,000-byte bound on the complete canonical request. The complete batch response is limited to 10 MiB; single responses retain their 4 MiB limit.

The gateway offers one aggregate USDC payment. Its accepted `extra.batchPricing` freezes the batch ID and each ordered job ID, model, output cap, pricing mode and retail amount. The client validates that manifest against the original request and exact aggregate charge before signing. Fresh short memos use the official 20,000-unit transaction; saved long memos retain their original authorization and 60,000-unit compatibility path. Both memo forms bind the aggregate quote ID and ordered request digest, including during standalone batch verification. Both the per-payment ceiling and cumulative ceiling apply to the whole batch; single and batch purchases share the same existing journal and cumulative budget.

A completed batch requires every child result, its original native job and operator receipt signatures, its model tariff when requested, and the ordered RFC6962 inclusion proofs. The paid quote's frozen batch and job IDs prevent substituting valid results from another purchase. Native operator tariffs remain separate from retail customer charges. Merkle consistency alone is not a signature or proof of model quality.

A `pending` response, partial result, failed child or uncertain transport outcome does not release the reservation or authorize another payment. Resume the exact original ordered request under its original UUID; the saved quote and authorization are reused even after prices change. This client does not purchase missing children individually or claim automatic refunds. Changing order, model, cap, pricing mode or content requires a separate purchase identity after accounting for any unresolved original purchase.

## Retry and spending semantics

Opt in to model-specific upfront quotes with `pricing: 'model_tariff_v1'` on a purchase or the application-configured LangChain/LangGraph tool. Omitting it preserves the legacy fixed-price request and signed result. A configured tariff combines a model's base price with the requested `max_tokens` output cap. `maxPaymentMicroUsdc` is a ceiling, not a fixed charge: the exact accepted quote is reserved and returned as `amountMicroUsdc`. This is upfront pricing for the requested cap, not billing or refunds based on tokens actually generated. Model, cap and pricing-mode changes require a new purchase UUID.

An opted-in result must carry the native tariff snapshot in both the signed job input and routing requirement. The client checks its model, revision, validity at job issue time, output cap and exact rounded native job price. A missing tariff is refused. The x402 customer payment is checked separately; the operator tariff does not prescribe a gateway markup. The same optional `pricing` setting is supported by `runSupportDemo` and saved in its workflow identity.

The SQLite journal synchronizes processes sharing the same journal file, buyer wallet and gateway. It reserves the quote amount before signing and saves the complete authorization before sending it. A retry must use the same UUID and identical request. Completed results are reused; incomplete requests resend the exact original authorization. A pending result or unknown network outcome never causes a fresh payment to be signed.

Tariff changes affect new quotes. A resumed purchase retains its original quoted amount, model, output cap and signed payment. Completed replay rechecks the saved quote, amount, signed request and receipt against the current configured identities without making a network request.

The cumulative ceiling includes **all reserved and completed purchases**, without a time reset. Failed or expired ambiguous authorizations stay reserved. Do not delete or replace the journal to retry. If an authorization expires before settlement, reconcile the transaction and gateway state before deciding on a separate purchase. A process that crashes while holding a request claim leaves it locked for up to five minutes; the money reservation remains afterward. This is conservative behavior, not automatic refund accounting.

Limits are expressed in integer micro-USDC (1,000,000 = 1 USDC). Use the same persistent journal in every process using this wallet and endpoint. Independent journal copies cannot enforce one shared ceiling. Temperature accepts increments of 0.01 to preserve JavaScript/Rust canonical request-hash interoperability.

## Official integration sources

- [Coinbase AgentKit](https://github.com/coinbase/agentkit)
- [Coinbase Solana keypair wallet](https://github.com/coinbase/agentkit/blob/main/typescript/agentkit/src/wallet-providers/solanaKeypairWalletProvider.ts)
- [x402 SVM implementation](https://github.com/x402-foundation/x402/tree/main/typescript/packages/mechanisms/svm)

This is an integration with open-source Coinbase AgentKit and the x402 protocol. It does not imply a Coinbase partnership or endorsement.
