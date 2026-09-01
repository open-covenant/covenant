import { createKeyPairSignerFromBytes } from '@solana/kit';
import { x402Facilitator } from '@x402/core/facilitator';
import { SettlementCache, toFacilitatorSvmSigner } from '@x402/svm';
import { loadConfig } from './config.js';
import { createLighthouseTolerantScheme } from './scheme.js';
import { createFacilitatorServer } from './server.js';

const config = loadConfig();
const feePayer = await createKeyPairSignerFromBytes(config.feePayerSecretKey);

// A pasted or rotated key that does not match the funded account would sign
// every settlement from the wrong address, so refuse to start instead.
if (feePayer.address !== config.feePayerPublicKey) {
  throw new Error(
    `Fee payer key does not match MIZUKI_FACILITATOR_FEE_PAYER_PUBLIC_KEY (key derives ${feePayer.address})`,
  );
}

const log = (event: Record<string, unknown>): void => console.log(JSON.stringify(event));
const facilitator = new x402Facilitator();
facilitator.register(
  [config.network],
  createLighthouseTolerantScheme(
    toFacilitatorSvmSigner(feePayer, { defaultRpcUrl: config.rpcUrl }),
    new SettlementCache(),
    log,
  ),
);

async function feePayerLamports(): Promise<number> {
  const response = await fetch(config.rpcUrl, {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify({
      jsonrpc: '2.0',
      id: 'facilitator-readiness',
      method: 'getBalance',
      params: [feePayer.address, { commitment: 'confirmed' }],
    }),
    signal: AbortSignal.timeout(10_000),
  });
  if (!response.ok) throw new Error(`RPC returned ${response.status}`);
  const body = (await response.json()) as { result?: { value?: unknown } };
  const value = body?.result?.value;
  if (typeof value !== 'number') throw new Error('RPC returned no balance');
  return value;
}

const server = createFacilitatorServer(
  {
    supported: () => facilitator.getSupported(),
    verify: (payload, requirements) => facilitator.verify(payload as never, requirements as never),
    settle: (payload, requirements) => facilitator.settle(payload as never, requirements as never),
    readiness: async () => ({ feePayerLamports: await feePayerLamports() }),
  },
  config,
  log,
);

server.listen(config.port, config.host, () => {
  log({
    event: 'facilitator_listening',
    host: config.host,
    port: config.port,
    network: config.network,
    feePayer: feePayer.address,
  });
});

for (const signal of ['SIGTERM', 'SIGINT'] as const) {
  process.on(signal, () => {
    server.close(() => process.exit(0));
  });
}
