import { createKeyPairSignerFromBytes } from '@solana/kit';
import { x402Facilitator } from '@x402/core/facilitator';
import { ExactEvmScheme } from '@x402/evm/exact/facilitator';
import { toFacilitatorEvmSigner } from '@x402/evm';
import { BAZAAR } from '@x402/extensions/bazaar';
import { SettlementCache, toFacilitatorSvmSigner } from '@x402/svm';
import { createWalletClient, http, nonceManager, publicActions } from 'viem';
import { privateKeyToAccount } from 'viem/accounts';
import { base, baseSepolia } from 'viem/chains';
import { BASE_MAINNET, BASE_SEPOLIA, SOLANA_MAINNET, USDC, loadConfig } from './config.js';
import { Gate } from './gate.js';
import { createLighthouseTolerantScheme } from './scheme.js';
import { createFacilitatorServer } from './server.js';
import { Store } from './store.js';

/** Below these a signer reports unready, so the operator hears before sellers do. */
const MIN_SOLANA_LAMPORTS = 5_000_000n;
const MIN_BASE_WEI = 200_000_000_000_000n;

/**
 * Each Solana settlement costs the fee payer two signatures plus the priority
 * fee the payer chose, so that fee is capped: a payer could otherwise set it
 * high enough to spend the float on one payment. At these caps a stock client
 * costs at most 10,000 lamports in priority fees and a guarded wallet 20,000.
 */
const SVM_FEE_CAPS = {
  maxComputeUnits: 100_000,
  maxPriorityFeeMicroLamports: 100_000,
  maxGuardedPriorityFeeLamports: 20_000n,
  maxRequiredSignatures: 2,
};

/** Below Render's and most sellers' request timeouts; slower Base receipts come back as settlement_pending. */
const BASE_CONFIRMATION_TIMEOUT_MS = 25_000;

const config = loadConfig();
const log = (event: Record<string, unknown>): void => console.log(JSON.stringify(event));
const facilitator = new x402Facilitator().registerExtension(BAZAAR);
const networks = new Set<string>();
const balances: Record<string, () => Promise<bigint>> = {};

if (config.solana) {
  const { rpcUrl, feePayerSecretKey, feePayerPublicKey } = config.solana;
  const feePayer = await createKeyPairSignerFromBytes(feePayerSecretKey);
  // A pasted or rotated key that does not match the published address would
  // sign every settlement from an account nobody funded, so refuse to start.
  if (feePayer.address !== feePayerPublicKey) {
    throw new Error(`Solana fee payer key derives ${feePayer.address}, not ${feePayerPublicKey}`);
  }
  facilitator.register(
    SOLANA_MAINNET,
    createLighthouseTolerantScheme(
      toFacilitatorSvmSigner(feePayer, { defaultRpcUrl: rpcUrl }),
      new SettlementCache(),
      log,
      SVM_FEE_CAPS,
    ),
  );
  networks.add(SOLANA_MAINNET);
  balances[SOLANA_MAINNET] = async () => {
    const response = await fetch(rpcUrl, {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify({
        jsonrpc: '2.0',
        id: 'readiness',
        method: 'getBalance',
        params: [feePayer.address, { commitment: 'confirmed' }],
      }),
      signal: AbortSignal.timeout(10_000),
    });
    if (!response.ok) throw new Error(`Solana RPC returned ${response.status}`);
    const value = ((await response.json()) as { result?: { value?: unknown } })?.result?.value;
    if (typeof value !== 'number') throw new Error('Solana RPC returned no balance');
    return BigInt(value);
  };
}

if (config.base) {
  const { network, rpcUrl, signerPrivateKey, signerAddress } = config.base;
  // Concurrent settlements need distinct nonces from one signer.
  const account = privateKeyToAccount(signerPrivateKey, { nonceManager });
  if (account.address.toLowerCase() !== signerAddress.toLowerCase()) {
    throw new Error(`Base signer key derives ${account.address}, not ${signerAddress}`);
  }
  const chain = network === BASE_SEPOLIA ? baseSepolia : base;
  const client = createWalletClient({ account, chain, transport: http(rpcUrl, { timeout: 15_000 }) }).extend(
    publicActions,
  );
  const signer = toFacilitatorEvmSigner(
    {
      address: account.address,
      getCode: (args) => client.getCode(args),
      readContract: (args) => client.readContract({ ...args, args: args.args ?? [] } as never),
      verifyTypedData: (args) => client.verifyTypedData(args as never),
      writeContract: (args) => client.writeContract({ ...args, args: args.args ?? [] } as never),
      sendTransaction: (args) => client.sendTransaction(args),
      waitForTransactionReceipt: (args) => client.waitForTransactionReceipt(args),
    },
    { confirmationTimeoutMs: BASE_CONFIRMATION_TIMEOUT_MS },
  );
  // Undeployed smart wallets (ERC-6492) stay refused: deploying one is a
  // contract call of the payer's choosing, paid for by this signer.
  facilitator.register(network, new ExactEvmScheme(signer));
  networks.add(network);
  balances[network] = () => client.getBalance({ address: account.address });
}

const store = new Store(config.dataDir);
const gate = new Gate(facilitator, store, config, networks, log);
const minimums: Record<string, bigint> = {
  [SOLANA_MAINNET]: MIN_SOLANA_LAMPORTS,
  [BASE_MAINNET]: MIN_BASE_WEI,
  [BASE_SEPOLIA]: MIN_BASE_WEI,
};

const server = createFacilitatorServer(
  {
    gate,
    store,
    supported: () => facilitator.getSupported(),
    about: () => ({
      name: 'Covenant x402 facilitator',
      x402Version: 2,
      url: config.publicUrl,
      fees: 'none',
      networks: [...networks].map((network) => ({ network, scheme: 'exact', asset: USDC[network] })),
      minimumAmount: config.minAmount.toString(),
      dailySettlementsPerReceiver: config.dailySettlementsPerPayTo,
      settlements: store.settlementTotals(),
      endpoints: ['/supported', '/verify', '/settle', '/discovery/resources', '/discovery/search'],
      source: 'https://github.com/open-covenant/covenant/tree/main/services/x402-facilitator',
    }),
    readiness: async () => {
      const results: Record<string, unknown> = {};
      let ok = true;
      for (const [network, balance] of Object.entries(balances)) {
        try {
          const value = await balance();
          const funded = value >= minimums[network]!;
          ok &&= funded;
          results[network] = { ok: funded, balance: value.toString() };
        } catch (error) {
          ok = false;
          results[network] = { ok: false };
          log({ event: 'facilitator_balance_unavailable', network, message: String(error) });
        }
      }
      return { ok, networks: results };
    },
  },
  config,
  log,
);

server.listen(config.port, config.host, () => {
  log({ event: 'facilitator_listening', port: config.port, networks: [...networks], signers: facilitator.getSupported().signers });
});

for (const signal of ['SIGTERM', 'SIGINT'] as const) {
  process.on(signal, () => {
    server.close(() => {
      store.close();
      process.exit(0);
    });
  });
}
