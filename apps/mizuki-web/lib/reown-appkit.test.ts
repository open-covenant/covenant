import { beforeEach, describe, expect, it, vi } from 'vitest';
import {
  address,
  assertIsSignatureBytes,
  blockhash,
  compileTransaction,
  createTransactionMessage,
  getBase58Decoder,
  getBase64Decoder,
  getTransactionDecoder,
  getTransactionEncoder,
  pipe,
  setTransactionMessageFeePayer,
  setTransactionMessageLifetimeUsingBlockhash,
} from '@solana/kit';
import type { WalletAccount } from '@wallet-standard/base';

const payer = '11111111111111111111111111111111';
const mainnet = 'solana:5eykt4UsFv8P8NJdTREpY1vzqKqZKvdp';

const mocks = vi.hoisted(() => {
  let open = false;
  const listeners = new Set<(state: { open: boolean }) => void>();
  const appKit = {
    open: vi.fn(async () => {
      open = true;
    }),
    close: vi.fn(async () => {
      open = false;
    }),
    isOpen: vi.fn(() => open),
    subscribeState: vi.fn((next: (state: { open: boolean }) => void) => {
      listeners.add(next);
      return () => listeners.delete(next);
    }),
    getProvider: vi.fn(),
    getProviderType: vi.fn(() => 'WALLET_CONNECT'),
  };
  const request = vi.fn();
  const rawProvider = {
    request,
    session: {
      namespaces: {
        solana: {
          accounts: ['solana:5eykt4UsFv8P8NJdTREpY1vzqKqZKvdp:11111111111111111111111111111111'],
          methods: ['solana_signTransaction'],
        },
      },
    },
  };
  const wrappedProvider = { provider: rawProvider, session: rawProvider.session };
  appKit.getProvider.mockReturnValue(rawProvider);
  return {
    adapter: vi.fn(),
    appKit,
    rawProvider,
    wrappedProvider,
    request,
    createAppKit: vi.fn((_options: unknown) => appKit),
    emitOpen(value: boolean) {
      open = value;
      for (const listener of listeners) listener({ open: value });
    },
    reset() {
      open = false;
      listeners.clear();
      request.mockReset();
      appKit.getProvider.mockReturnValue(rawProvider);
      appKit.getProviderType.mockReturnValue('WALLET_CONNECT');
      rawProvider.session.namespaces.solana.accounts = [
        'solana:5eykt4UsFv8P8NJdTREpY1vzqKqZKvdp:11111111111111111111111111111111',
      ];
      rawProvider.session.namespaces.solana.methods = ['solana_signTransaction'];
    },
  };
});

vi.mock('@reown/appkit/react', () => ({ createAppKit: mocks.createAppKit }));
vi.mock('@reown/appkit/networks', () => ({
  solana: { id: 'mainnet' },
  solanaDevnet: { id: 'devnet' },
}));
vi.mock('@reown/appkit-adapter-solana/react', () => ({
  SolanaAdapter: class {
    constructor(options: unknown) {
      mocks.adapter(options);
    }
  },
}));

describe('Reown AppKit', () => {
  beforeEach(() => mocks.reset());

  it('initializes one constrained Solana Wallet Standard bridge', async () => {
    const first = await import('./reown-appkit');
    const second = await import('./reown-appkit');
    const options = mocks.createAppKit.mock.calls[0]?.[0] as Record<string, unknown>;

    expect(first).toBe(second);
    expect(mocks.createAppKit).toHaveBeenCalledOnce();
    expect(mocks.adapter).toHaveBeenCalledWith({ registerWalletStandard: true });
    expect(first.reownProjectId).toBe('b12827dfd1ff27064b91c710188bdbe4');
    expect(options).toMatchObject({
      projectId: 'b12827dfd1ff27064b91c710188bdbe4',
      networks: [{ id: 'mainnet' }],
      defaultNetwork: { id: 'mainnet' },
      enableNetworkSwitch: false,
      manualWCControl: true,
      termsConditionsUrl: 'https://mizuki.opencovenant.org/terms',
      privacyPolicyUrl: 'https://mizuki.opencovenant.org/privacy',
      metadata: {
        url: 'https://mizuki.opencovenant.org',
        icons: ['https://mizuki.opencovenant.org/mizuki-icon-180.png'],
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
    });
  });

  it('releases a closed attempt without letting it close or cancel a later attempt', async () => {
    const { connectWithWalletConnect } = await import('./reown-appkit');
    let resolveFirst!: (value: string) => void;
    const firstConnection = new Promise<string>((done) => {
      resolveFirst = done;
    });
    const cancelFirst = vi.fn();
    const first = connectWithWalletConnect(() => firstConnection, cancelFirst, vi.fn());

    await vi.waitFor(() => expect(mocks.appKit.open).toHaveBeenCalled());
    mocks.emitOpen(false);
    await expect(first).resolves.toBeNull();
    expect(cancelFirst).toHaveBeenCalledOnce();

    let resolveSecond!: (value: string) => void;
    const secondConnection = new Promise<string>((done) => {
      resolveSecond = done;
    });
    const cancelSecond = vi.fn();
    const second = connectWithWalletConnect(() => secondConnection, cancelSecond, vi.fn());
    await vi.waitFor(() => expect(mocks.appKit.open).toHaveBeenCalledTimes(2));
    expect(mocks.appKit.isOpen()).toBe(true);

    resolveFirst('stale');
    await Promise.resolve();
    expect(mocks.appKit.isOpen()).toBe(true);
    expect(cancelSecond).not.toHaveBeenCalled();

    resolveSecond('connected');
    await expect(second).resolves.toBe('connected');
    expect(mocks.appKit.close).toHaveBeenCalledOnce();
  });

  it('reports a safe error when the modal cannot open', async () => {
    const { connectWithWalletConnect } = await import('./reown-appkit');
    const connect = vi.fn();
    const fail = vi.fn();
    mocks.appKit.open.mockRejectedValueOnce(new Error('relay project secret'));

    await expect(connectWithWalletConnect(connect, vi.fn(), fail)).resolves.toBeNull();

    expect(connect).not.toHaveBeenCalled();
    expect(fail).toHaveBeenCalledWith(
      'WalletConnect could not open a secure session. Reopen the wallet and try again.',
    );
    expect(fail.mock.calls[0]?.[0]).not.toContain('secret');
  });

  it('signs an actual version-zero transaction from a base58 signature response', async () => {
    const { signWalletConnectTransactions } = await import('./reown-appkit');
    const transaction = versionedTransaction();
    const signatureBytes = new Uint8Array(64).fill(7);
    mocks.request.mockResolvedValue({
      signature: getBase58Decoder().decode(signatureBytes),
    });

    const [result] = await signWalletConnectTransactions({
      account: walletAccount(),
      chain: 'solana:mainnet',
      transaction,
    });
    const decoded = getTransactionDecoder().decode(result!.signedTransaction);

    expect(decoded.messageBytes[0]).toBe(128);
    expect(decoded.signatures[address(payer)]).toEqual(signatureBytes);
    expect(mocks.request).toHaveBeenCalledWith(
      {
        method: 'solana_signTransaction',
        params: { transaction: getBase64Decoder().decode(transaction), pubkey: payer },
      },
      mainnet,
    );
  });

  it('preserves the full wallet transaction for downstream payment validation', async () => {
    const { signWalletConnectTransactions } = await import('./reown-appkit');
    const originalBytes = versionedTransaction();
    const original = getTransactionDecoder().decode(originalBytes);
    const signatureBytes = new Uint8Array(64).fill(9);
    assertIsSignatureBytes(signatureBytes);
    const signedBytes = getTransactionEncoder().encode({
      ...original,
      signatures: { ...original.signatures, [address(payer)]: signatureBytes },
    });
    mocks.request.mockResolvedValue({
      transaction: getBase64Decoder().decode(signedBytes),
    });

    const [result] = await signWalletConnectTransactions({
      account: walletAccount(),
      chain: 'solana:mainnet',
      transaction: originalBytes,
    });
    expect(result!.signedTransaction).toEqual(signedBytes);

    const changed = getTransactionDecoder().decode(
      versionedTransaction('SysvarRent111111111111111111111111111111111'),
    );
    const changedBytes = getTransactionEncoder().encode({
      ...changed,
      signatures: { ...changed.signatures, [address(payer)]: signatureBytes },
    });
    mocks.request.mockResolvedValue({
      transaction: getBase64Decoder().decode(changedBytes),
    });
    await expect(
      signWalletConnectTransactions({
        account: walletAccount(),
        chain: 'solana:mainnet',
        transaction: originalBytes,
      }),
    ).resolves.toEqual([{ signedTransaction: changedBytes }]);
  });

  it('attaches a detached signature to a wallet-returned transaction', async () => {
    const { signWalletConnectTransactions } = await import('./reown-appkit');
    const transaction = versionedTransaction();
    const signatureBytes = new Uint8Array(64).fill(6);
    mocks.request.mockResolvedValue({
      transaction: getBase64Decoder().decode(transaction),
      signature: getBase58Decoder().decode(signatureBytes),
    });

    const [result] = await signWalletConnectTransactions({
      account: walletAccount(),
      chain: 'solana:mainnet',
      transaction,
    });
    const decoded = getTransactionDecoder().decode(result!.signedTransaction);

    expect(decoded.signatures[address(payer)]).toEqual(signatureBytes);
  });

  it('uses the underlying requester when AppKit returns a wrapped reconnect provider', async () => {
    const { signWalletConnectTransactions } = await import('./reown-appkit');
    mocks.appKit.getProvider.mockReturnValue(mocks.wrappedProvider);
    mocks.request.mockResolvedValue({
      signature: getBase58Decoder().decode(new Uint8Array(64).fill(5)),
    });

    await signWalletConnectTransactions({
      account: walletAccount(),
      chain: 'solana:mainnet',
      transaction: versionedTransaction(),
    });

    expect(mocks.request).toHaveBeenCalledOnce();
  });

  it('rejects a provider or session that does not match the selected account', async () => {
    const { signWalletConnectTransactions } = await import('./reown-appkit');
    mocks.appKit.getProviderType.mockReturnValue('INJECTED');

    await expect(
      signWalletConnectTransactions({
        account: walletAccount(),
        chain: 'solana:mainnet',
        transaction: versionedTransaction(),
      }),
    ).rejects.toThrow('not connected');

    mocks.appKit.getProviderType.mockReturnValue('WALLET_CONNECT');
    mocks.rawProvider.session.namespaces.solana.accounts = [];
    await expect(
      signWalletConnectTransactions({
        account: walletAccount(),
        chain: 'solana:mainnet',
        transaction: versionedTransaction(),
      }),
    ).rejects.toThrow('does not match');
  });
});

function versionedTransaction(lifetime = payer): Uint8Array {
  const message = pipe(
    createTransactionMessage({ version: 0 }),
    (value) => setTransactionMessageFeePayer(address(payer), value),
    (value) =>
      setTransactionMessageLifetimeUsingBlockhash(
        { blockhash: blockhash(lifetime), lastValidBlockHeight: 1n },
        value,
      ),
  );
  return new Uint8Array(getTransactionEncoder().encode(compileTransaction(message)));
}

function walletAccount(): WalletAccount {
  return {
    address: payer,
    publicKey: new Uint8Array(32),
    chains: ['solana:mainnet'],
    features: ['solana:signTransaction'],
  };
}
