import { ActionProvider, SolanaKeypairWalletProvider } from '@coinbase/agentkit';
import { z } from 'zod';
import { CovenantX402Client, requestSchema, SOLANA_GENESIS } from './index.mjs';

const actionSchema = requestSchema.extend({ requestId: z.string().uuid() });
// AgentKit 0.10.4 starts an unawaited analytics request from this hook;
// a non-2xx analytics response becomes an unhandled rejection. This local
// demo wallet disables only that hook; key custody and signing are AgentKit's.
export class CovenantSolanaKeypairWalletProvider extends SolanaKeypairWalletProvider {
  trackInitialization() {}
}
export class CovenantInferenceActionProvider extends ActionProvider {
  constructor(config) { super('covenant-inference', []); this.config = config; this.clients = new WeakMap(); }
  supportsNetwork(network) { return network.protocolFamily === 'svm' && network.chainId === SOLANA_GENESIS; }
  getActions(walletProvider) {
    let client = this.clients.get(walletProvider);
    if (!client) { client = new CovenantX402Client({ ...this.config, walletProvider }); this.clients.set(walletProvider, client); }
    return [{ name: 'covenant_inference',
      description: 'Buy an inference response from Covenant with this AgentKit wallet, paying Solana USDC within configured limits. Reuse the same requestId and exact input to resume a pending purchase. Returns only a confirmed payment and cryptographically verified Compute receipt, or pending.',
      schema: actionSchema,
      invoke: async args => JSON.stringify(await client.purchase(actionSchema.parse(args))),
    }];
  }
  close(walletProvider) { this.clients.get(walletProvider)?.close(); this.clients.delete(walletProvider); }
}
export const covenantInferenceActionProvider = config => new CovenantInferenceActionProvider(config);
