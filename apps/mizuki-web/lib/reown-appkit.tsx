'use client';

import { createAppKit } from '@reown/appkit/react';
import { solana, solanaDevnet } from '@reown/appkit/networks';
import { SolanaAdapter } from '@reown/appkit-adapter-solana/react';
import {
  address,
  assertIsSignatureBytes,
  getBase58Encoder,
  getBase64Decoder,
  getBase64Encoder,
  getTransactionDecoder,
  getTransactionEncoder,
} from '@solana/kit';
import type {
  SolanaSignTransactionInput,
  SolanaSignTransactionOutput,
} from '@solana/wallet-standard-features';

export const reownProjectId =
  process.env.NEXT_PUBLIC_REOWN_PROJECT_ID?.trim() || 'b12827dfd1ff27064b91c710188bdbe4';

const appUrl = 'https://mizuki.opencovenant.org';
const network = process.env.NEXT_PUBLIC_SOLANA_NETWORK === 'solana-devnet' ? solanaDevnet : solana;
let modalAttempt = 0;
const appKit = createAppKit({
  adapters: [new SolanaAdapter({ registerWalletStandard: true })],
  projectId: reownProjectId,
  networks: [network],
  defaultNetwork: network,
  metadata: {
    name: 'Mizuki the Mech',
    description: 'Fixed-price maintenance for public GitHub repositories.',
    url: appUrl,
    icons: [`${appUrl}/mizuki-icon-180.png`],
  },
  features: {
    analytics: false,
    email: false,
    socials: false,
    swaps: false,
    onramp: false,
    receive: false,
    send: false,
    history: false,
    pay: false,
    smartSessions: false,
    reownAuthentication: false,
  },
  enableNetworkSwitch: false,
  manualWCControl: true,
  termsConditionsUrl: `${appUrl}/terms`,
  privacyPolicyUrl: `${appUrl}/privacy`,
  themeMode: 'dark',
  themeVariables: {
    '--w3m-color-mix': '#030303',
    '--w3m-color-mix-strength': 40,
    '--w3m-accent': '#f5f5f5',
    '--w3m-font-family': 'ui-sans-serif, system-ui, -apple-system, "Segoe UI", Roboto, sans-serif',
    '--w3m-border-radius-master': '2px',
  },
});

type WalletConnectSession = {
  namespaces?: {
    solana?: { accounts?: readonly string[]; methods?: readonly string[] };
  };
};

type WalletConnectRequester = {
  request(
    input: {
      method: 'solana_signTransaction';
      params: { transaction: string; pubkey: string };
    },
    chain: string,
  ): Promise<unknown>;
  session?: WalletConnectSession;
};

type WalletConnectProvider =
  | WalletConnectRequester
  | {
      provider: WalletConnectRequester;
      session?: WalletConnectSession;
    };

export async function connectWithWalletConnect<T>(
  connect: () => Promise<T>,
  cancel: () => void,
  fail: (message: string) => void,
): Promise<T | null> {
  const attempt = ++modalAttempt;
  try {
    await appKit.open({ view: 'ConnectingWalletConnectBasic', namespace: 'solana' });
  } catch {
    fail('WalletConnect could not open a secure session. Reopen the wallet and try again.');
    return null;
  }
  let settled = false;
  let opened = appKit.isOpen();
  let closeAttempt!: () => void;
  const closed = new Promise<null>((resolve) => {
    closeAttempt = () => resolve(null);
  });
  const unsubscribe = appKit.subscribeState((state) => {
    if (state.open) opened = true;
    if (opened && !state.open && !settled) {
      cancel();
      closeAttempt();
    }
  });

  try {
    return await Promise.race([connect(), closed]);
  } finally {
    settled = true;
    unsubscribe();
    if (attempt === modalAttempt && appKit.isOpen()) await appKit.close();
  }
}

export async function signWalletConnectTransactions(
  ...inputs: readonly SolanaSignTransactionInput[]
): Promise<readonly SolanaSignTransactionOutput[]> {
  const provider = appKit.getProvider<WalletConnectProvider>('solana');
  if (appKit.getProviderType('solana') !== 'WALLET_CONNECT' || !provider) {
    throw new Error('WalletConnect is not connected');
  }
  const { rpc, session } = resolveWalletConnectProvider(provider);

  const outputs: SolanaSignTransactionOutput[] = [];
  for (const input of inputs) {
    validateSigningAccount(session, input);
    const response = await rpc.request(
      {
        method: 'solana_signTransaction',
        params: {
          transaction: getBase64Decoder().decode(input.transaction),
          pubkey: input.account.address,
        },
      },
      walletConnectChain(input),
    );
    outputs.push({ signedTransaction: readSignedTransaction(input, response) });
  }
  return outputs;
}

function resolveWalletConnectProvider(provider: WalletConnectProvider): {
  rpc: WalletConnectRequester;
  session: WalletConnectSession;
} {
  const rpc = 'provider' in provider ? provider.provider : provider;
  const session = provider.session ?? rpc.session;
  if (!session) throw new Error('WalletConnect is not connected');
  return { rpc, session };
}

function validateSigningAccount(
  session: WalletConnectSession,
  input: SolanaSignTransactionInput,
): void {
  const accounts = session.namespaces?.solana?.accounts ?? [];
  const methods = session.namespaces?.solana?.methods ?? [];
  const chain = walletConnectChain(input);
  if (!accounts.includes(`${chain}:${input.account.address}`)) {
    throw new Error('WalletConnect account does not match the payment account');
  }
  if (input.chain && !input.account.chains.includes(input.chain)) {
    throw new Error('WalletConnect account does not support the requested Solana network');
  }
  if (!methods.includes('solana_signTransaction')) {
    throw new Error('WalletConnect wallet does not support transaction signing');
  }
}

function walletConnectChain(input: SolanaSignTransactionInput): string {
  if (input.chain === 'solana:mainnet') return 'solana:5eykt4UsFv8P8NJdTREpY1vzqKqZKvdp';
  if (input.chain === 'solana:devnet') return 'solana:EtWTRABZaYq6iMfeYKouRu166VU2xqa1';
  throw new Error('WalletConnect requires an explicit supported Solana network');
}

function readSignedTransaction(input: SolanaSignTransactionInput, response: unknown): Uint8Array {
  if (!isRecord(response)) throw new Error('WalletConnect returned an invalid signing response');

  const decoder = getTransactionDecoder();
  const encoder = getTransactionEncoder();
  const original = decoder.decode(input.transaction);

  if (typeof response.transaction === 'string') {
    const bytes = getBase64Encoder().encode(response.transaction);
    const signed = decoder.decode(bytes);
    const signer = address(input.account.address);
    const signature = signed.signatures[signer];
    if (!signature || signature.every((byte) => byte === 0)) {
      throw new Error('WalletConnect did not sign the payment transaction');
    }
    return new Uint8Array(bytes);
  }

  if (typeof response.signature === 'string') {
    const signature = getBase58Encoder().encode(response.signature);
    assertIsSignatureBytes(signature);
    const signer = address(input.account.address);
    if (!(signer in original.signatures)) {
      throw new Error('WalletConnect account is not a signer for this transaction');
    }
    return new Uint8Array(
      encoder.encode({
        ...original,
        signatures: { ...original.signatures, [signer]: signature },
      }),
    );
  }

  throw new Error('WalletConnect returned an invalid signing response');
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null && !Array.isArray(value);
}
