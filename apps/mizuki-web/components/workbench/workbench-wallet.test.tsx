import { renderToStaticMarkup } from 'react-dom/server';
import { describe, expect, it, vi } from 'vitest';
import type { CompatibleWallet } from '@/lib/wallet-standard';
import {
  useWorkbenchWallet,
  WorkbenchWalletControl,
  WorkbenchWalletControlView,
  WorkbenchWalletProvider,
  type WorkbenchWalletSession,
} from './workbench-wallet';

const mocks = vi.hoisted(() => ({ useStandardWallet: vi.fn() }));

vi.mock('@/lib/wallet-standard', () => ({
  paymentWalletNetwork: () => ({ chain: 'solana:mainnet', label: 'Solana mainnet' }),
  useStandardWallet: mocks.useStandardWallet,
}));

describe('Workbench wallet control', () => {
  it('offers every compatible wallet from the shared Wallet Standard session', () => {
    const html = renderToStaticMarkup(
      <WorkbenchWalletControlView
        {...session({ wallets: [wallet('WalletConnect'), wallet('Browser Wallet')] })}
      />,
    );

    expect(html).toContain('aria-label="Connect payment wallet"');
    expect(html).toContain('Connect wallet');
    expect(html).toContain('Choose a Solana wallet');
    expect(html).toContain('WalletConnect');
    expect(html).toContain('Scan a QR code or open a mobile wallet');
    expect(html).toContain('Browser Wallet');
  });

  it('shows the exact connected account context and a disconnect action', () => {
    const address = '2n1fD5H61zB8Qsg9iswBteVPWzr3pzWEeTXejXtuTn2E';
    const activeWallet = wallet('WalletConnect');
    const html = renderToStaticMarkup(
      <WorkbenchWalletControlView
        {...session({
          connected: {
            wallet: activeWallet,
            account: {
              address,
              publicKey: new Uint8Array(32),
              chains: ['solana:mainnet'],
              features: ['solana:signTransaction'],
            },
          },
          ready: true,
        })}
      />,
    );

    expect(html).toContain(`aria-label="Payment wallet ${address}"`);
    expect(html).toContain(`title="${address}"`);
    expect(html).toContain('Wallet connected');
    expect(html).toContain('Solana mainnet');
    expect(html).toContain('Change wallet');
    expect(html).toContain('Disconnect');
  });

  it('provides one connection lifecycle to the header control and payment consumer', () => {
    const shared = session({
      connected: {
        wallet: wallet('WalletConnect'),
        account: {
          address: 'shared-payment-account',
          publicKey: new Uint8Array(32),
          chains: ['solana:mainnet'],
          features: ['solana:signTransaction'],
        },
      },
      ready: true,
    });
    mocks.useStandardWallet.mockReturnValue(shared);

    const html = renderToStaticMarkup(
      <WorkbenchWalletProvider>
        <WorkbenchWalletControl />
        <WalletConsumer name="payment" />
      </WorkbenchWalletProvider>,
    );

    expect(mocks.useStandardWallet).toHaveBeenCalledOnce();
    expect(mocks.useStandardWallet).toHaveBeenCalledWith('transaction');
    expect(html).toContain('aria-label="Payment wallet shared-payment-account"');
    expect(html).toContain('data-consumer="payment"');
    expect(html.match(/shared-payment-account/g)?.length).toBeGreaterThanOrEqual(2);
  });
});

function WalletConsumer({ name }: { name: string }) {
  const wallet = useWorkbenchWallet();
  return <span data-consumer={name}>{wallet.connected?.account.address}</span>;
}

function session(overrides: Partial<WorkbenchWalletSession> = {}): WorkbenchWalletSession {
  return {
    wallets: [],
    connected: null,
    ready: false,
    connecting: null,
    error: null,
    connect: vi.fn(async () => null),
    disconnect: vi.fn(async () => undefined),
    ...overrides,
  } as WorkbenchWalletSession;
}

function wallet(name: string): CompatibleWallet {
  return {
    version: '1.0.0',
    name,
    icon: 'data:image/svg+xml;base64,',
    chains: ['solana:mainnet'],
    accounts: [],
    features: {},
  } as CompatibleWallet;
}
