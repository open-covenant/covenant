import reconciliation from "../../public/compute/langchain/reconciliation.json";

const source = "https://github.com/open-covenant/covenant/blob/9e53e687b0ed483dca9b66328e70c43f0c9aecdf";

// Accepted installed package and funded workflow. The immutable release asset
// was independently downloaded without authentication and its hash checked.
export const langchainRelease = {
  accepted: true,
  version: "0.2.0",
  packageUrl: "https://github.com/open-covenant/covenant/releases/download/codex-compute-x402-client-v0.2.0/covenant-compute-x402-client-0.2.0.tgz",
  packageSha256: "77ef86146f982432f75a83d227afb8c7a8f36049bb23d4eba29ae0da0cb606cf",
  releaseUrl: "https://github.com/open-covenant/covenant/releases/tag/codex-compute-x402-client-v0.2.0",
  sourceUrl: `${source}/sdk/compute-x402-client/langchain.mjs`,
  documentationUrl: `${source}/sdk/compute-x402-client/README.md#register-with-langchain-or-langgraph`,
  demoSourceUrl: `${source}/sdk/compute-x402-client/langgraph-demo.mjs`,
  configurationUrl: "/compute/inference/agentkit-config.example.json",
  example: `import { createAgent } from 'langchain';

// Use your existing planner and a persistent graph checkpointer.
const agent = createAgent({
  model: planner,
  tools: [compute],
  checkpointer,
});

const result = await agent.invoke({ messages: [{
  role: 'user',
  content: 'Sample ticket: my plan change shows two charges and I cannot '
    + 'download the invoice. Triage it and draft a reply for review.',
}] }, { configurable: { thread_id: threadId } });

const purchase = result.messages
  .findLast(message => message.name === 'covenant_inference')?.artifact;
if (purchase?.status === 'completed') {
  console.log(purchase.text); // Full signed evidence is retained in purchase.
}`,
  acceptance: {
    evidenceUrl: `${source}/docs/compute-langchain-release-evidence.md#funded-acceptance--2026-10-18`,
    paymentTransaction: reconciliation.transaction,
    customerPaymentUsdc: (reconciliation.amountMicroUsdc / 1_000_000).toFixed(2),
    completedAt: "18 Oct 2026 · 15:07 UTC",
  },
};
