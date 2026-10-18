# Covenant inference for Coinbase AgentKit

An AgentKit action that buys inference from Covenant with Solana USDC using standard x402 V2. The wallet provided to `AgentKit.from` signs the actual SPL-token transfer through `walletProvider.signTransaction`. The package does not use a second wallet or a separate CDP client.

This client is a release candidate until the deployment evidence records a live purchase. Local tests use isolated, unfunded wallets and a local RPC fixture.

## Install

Requires Node.js 24 or later (the durable journal uses Node's SQLite implementation).

Until a registry release is published, install the checked source package from
this release branch:

```sh
git clone --filter=blob:none --branch codex/compute-x402-release \
  https://github.com/open-covenant/covenant.git covenant-x402
cd covenant-x402/sdk/compute-x402-client
npm ci --ignore-scripts
npm test
npm pack

# In the application that will use AgentKit:
npm install /absolute/path/to/covenant-compute-x402-client-0.1.0.tgz
```

The packaged `demo.mjs` is also available at
`node_modules/@covenant/compute-x402-client/demo.mjs` after installation.

Direct dependencies and the complete dependency tree are locked. The Coinbase AgentKit dependency is `0.10.4`; official `@x402/core` and `@x402/svm` are `2.28.0`. AgentKit has a large dependency tree; its optional native bigint acceleration is not required.

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
  messages: [{ role: 'user', content: 'Return a complete, self-contained HTML webpage for an interactive meeting cost calculator. Include CSS and JavaScript inline, no external resources. Return HTML only.' }],
  max_tokens: 2048,
  temperature: 0.2,
}));
provider.close(walletProvider);
```

`walletProvider` must expose AgentKit's Solana transaction-signing interface on mainnet. The demo uses `CovenantSolanaKeypairWalletProvider`, a subclass of Coinbase's `SolanaKeypairWalletProvider`. It disables only the upstream initialization analytics hook, whose unawaited HTTP error can terminate Node in AgentKit 0.10.4. All custody and signing behavior remains the original AgentKit implementation. The official Solana Kit modifying-signer interface raises the unsigned x402 transaction’s compute limit from 20,000 to a fixed 60,000 units before the wallet signs. The transfer with its request-bound memo consumed 43,642 units in a read-only mainnet simulation. The adapter changes only that instruction, rejects transactions that already carry signatures, and rejects any further message change by the wallet. Transfer, memo, priority fee and blockhash remain unchanged.

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

## Retry and spending semantics

The SQLite journal synchronizes processes sharing the same journal file, buyer wallet and gateway. It reserves the quote amount before signing and saves the complete authorization before sending it. A retry must use the same UUID and identical request. Completed results are reused; incomplete requests resend the exact original authorization. A pending result or unknown network outcome never causes a fresh payment to be signed.

The cumulative ceiling includes **all reserved and completed purchases**, without a time reset. Failed or expired ambiguous authorizations stay reserved. Do not delete or replace the journal to retry. If an authorization expires before settlement, reconcile the transaction and gateway state before deciding on a separate purchase. A process that crashes while holding a request claim leaves it locked for up to five minutes; the money reservation remains afterward. This is conservative behavior, not automatic refund accounting.

Limits are expressed in integer micro-USDC (1,000,000 = 1 USDC). Use the same persistent journal in every process using this wallet and endpoint. Independent journal copies cannot enforce one shared ceiling. Temperature accepts increments of 0.01 to preserve JavaScript/Rust canonical request-hash interoperability.

## Official integration sources

- [Coinbase AgentKit](https://github.com/coinbase/agentkit)
- [Coinbase Solana keypair wallet](https://github.com/coinbase/agentkit/blob/main/typescript/agentkit/src/wallet-providers/solanaKeypairWalletProvider.ts)
- [x402 SVM implementation](https://github.com/x402-foundation/x402/tree/main/typescript/packages/mechanisms/svm)

This is an integration with open-source Coinbase AgentKit and the x402 protocol. It does not imply a Coinbase partnership or endorsement.
