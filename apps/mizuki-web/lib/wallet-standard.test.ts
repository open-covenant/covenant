import type { WalletAccount } from '@wallet-standard/base';
import { describe, expect, it, vi } from 'vitest';
import {
  observeWalletAccounts,
  paymentWalletNetwork,
  selectSolanaAccount,
  supportsWallet,
  type CompatibleWallet,
  type WalletConnectionState,
  WalletConnectionController,
} from './wallet-standard';

describe('Wallet Standard account observation', () => {
  it('subscribes before use and reports account switches and disconnects', () => {
    let change: ((value: { accounts?: readonly WalletAccount[] }) => void) | undefined;
    const off = vi.fn();
    const on = vi.fn((_event: 'change', listener: typeof change) => {
      change = listener;
      return off;
    });
    const wallet = {
      features: {
        'standard:events': {
          version: '1.0.0',
          on,
        },
      },
    } as unknown as CompatibleWallet;
    const update = vi.fn();
    const first = account('first');
    const second = account('second');

    const unsubscribe = observeWalletAccounts(wallet, update);
    change?.({ accounts: [first, second] });
    change?.({ accounts: [] });
    unsubscribe();

    expect(on).toHaveBeenCalledWith('change', expect.any(Function));
    expect(update).toHaveBeenNthCalledWith(1, first);
    expect(update).toHaveBeenNthCalledWith(2, null);
    expect(off).toHaveBeenCalledOnce();
  });

  it('ignores non-Solana accounts and requires change events', () => {
    expect(selectSolanaAccount([account('evm', ['eip155:1'])])).toBeNull();
    expect(() => observeWalletAccounts({ features: {} } as CompatibleWallet, vi.fn())).toThrow(
      'cannot report account or disconnect changes',
    );
  });

  it('selects only an account on the configured payment chain', () => {
    const devnet = account('devnet', ['solana:devnet']);
    const mainnet = account('mainnet', ['solana:mainnet']);

    expect(selectSolanaAccount([devnet, mainnet], 'solana:mainnet')).toBe(mainnet);
    expect(selectSolanaAccount([devnet], 'solana:mainnet')).toBeNull();
  });

  it('selects an account that supports the required operation', () => {
    const readOnly = account('read-only', ['solana:mainnet'], []);
    const signing = account('signing', ['solana:mainnet'], ['solana:signTransaction']);

    expect(selectSolanaAccount([readOnly, signing], 'solana:mainnet', 'transaction')).toBe(signing);
    expect(selectSolanaAccount([readOnly], 'solana:mainnet', 'transaction')).toBeNull();
  });

  it('does not advertise a wallet that cannot sign version-zero payments', () => {
    const base = wallet('Versioned', Promise.resolve()).wallet;
    const compatible = {
      ...base,
      features: {
        ...base.features,
        'solana:signTransaction': {
          version: '1.0.0',
          supportedTransactionVersions: [0],
          signTransaction: vi.fn(),
        },
      },
    } as CompatibleWallet;
    expect(supportsWallet(compatible, 'transaction', 'solana:mainnet')).toBe(true);

    const legacy = {
      ...base,
      features: {
        ...base.features,
        'solana:signTransaction': {
          version: '1.0.0',
          supportedTransactionVersions: ['legacy'],
          signTransaction: vi.fn(),
        },
      },
    } as CompatibleWallet;
    expect(supportsWallet(legacy, 'transaction', 'solana:mainnet')).toBe(false);
  });

  it('derives the payment label from the configured Solana network', () => {
    vi.stubEnv('NEXT_PUBLIC_SOLANA_NETWORK', 'solana-devnet');
    try {
      expect(paymentWalletNetwork()).toEqual({
        chain: 'solana:devnet',
        label: 'Solana devnet',
      });
    } finally {
      vi.unstubAllEnvs();
    }
  });

  it('starts a replacement connection immediately and ignores stale completion state', async () => {
    const firstConnect = deferred<void>();
    const secondConnect = deferred<void>();
    const first = wallet('First', firstConnect.promise);
    const second = wallet('Second', secondConnect.promise);
    const { controller, state } = connectionController();

    const firstOperation = controller.connect(first.wallet);
    await vi.waitFor(() => expect(first.connect).toHaveBeenCalledOnce());
    const secondOperation = controller.connect(second.wallet);
    await vi.waitFor(() => expect(second.connect).toHaveBeenCalledOnce());
    expect(state.current).toMatchObject({ connected: null, connecting: 'Second', error: null });

    second.setAccounts([account('second')]);
    secondConnect.resolve();
    await secondOperation;
    expect(state.current.connected?.wallet).toBe(second.wallet);
    expect(state.current.connected?.account.address).toBe('second');

    first.setAccounts([account('first')]);
    firstConnect.resolve();
    await firstOperation;
    expect(first.disconnect).toHaveBeenCalledOnce();
    expect(state.current).toMatchObject({ ready: true, connecting: null, error: null });
  });

  it('subscribes before re-reading accounts and follows account changes', async () => {
    const connection = deferred<void>();
    const connected = wallet('Wallet', connection.promise);
    const { controller, state } = connectionController();

    const operation = controller.connect(connected.wallet);
    connected.setAccounts([account('fresh')]);
    connection.resolve();
    await operation;

    expect(connected.events).toHaveBeenCalledOnce();
    expect(state.current.connected?.account.address).toBe('fresh');
    connected.change([account('switched')]);
    expect(state.current.connected?.account.address).toBe('switched');
    connected.change([]);
    expect(state.current).toMatchObject({ connected: null, ready: false });
    await controller.disconnect();
    expect(connected.disconnect).toHaveBeenCalledOnce();
  });

  it('does not let a stale disconnect failure overwrite a newer connection', async () => {
    const disconnect = deferred<void>();
    const first = wallet('First', Promise.resolve(), disconnect.promise);
    const second = wallet('Second', Promise.resolve());
    first.setAccounts([account('first')]);
    second.setAccounts([account('second')]);
    const { controller, state } = connectionController();

    await controller.connect(first.wallet);
    const disconnectOperation = controller.disconnect();
    const secondOperation = controller.connect(second.wallet);
    disconnect.reject(new Error('stale disconnect failure'));
    await disconnectOperation;
    await secondOperation;

    expect(state.current.connected?.wallet).toBe(second.wallet);
    expect(state.current).toMatchObject({ ready: true, connecting: null, error: null });
  });

  it('cancels an unfinished WalletConnect session and starts another wallet immediately', async () => {
    const connection = deferred<void>();
    const pending = wallet('WalletConnect', connection.promise);
    const replacement = wallet('Replacement', Promise.resolve());
    replacement.setAccounts([account('replacement')]);
    const { controller, state } = connectionController();

    const operation = controller.connect(pending.wallet);
    await vi.waitFor(() => expect(pending.connect).toHaveBeenCalledOnce());
    controller.cancelPending();

    expect(state.current).toMatchObject({ connected: null, ready: false, connecting: null });
    expect(pending.disconnect).toHaveBeenCalledOnce();

    const replacementOperation = controller.connect(replacement.wallet);
    await vi.waitFor(() => expect(replacement.connect).toHaveBeenCalledOnce());
    await replacementOperation;
    expect(state.current.connected?.wallet).toBe(replacement.wallet);

    pending.setAccounts([account('late-account')]);
    connection.resolve();
    await operation;

    expect(pending.disconnect).toHaveBeenCalledTimes(2);
    expect(state.current.connected?.wallet).toBe(replacement.wallet);
    expect(state.current).toMatchObject({ ready: true, connecting: null, error: null });
  });

  it('waits for a cancelled WalletConnect teardown before retrying the same wallet', async () => {
    const connection = deferred<void>();
    const teardown = deferred<void>();
    const pending = wallet('WalletConnect', connection.promise, teardown.promise);
    const { controller, state } = connectionController();

    const first = controller.connect(pending.wallet);
    await vi.waitFor(() => expect(pending.connect).toHaveBeenCalledOnce());
    controller.cancelPending();
    const retry = controller.connect(pending.wallet);

    await Promise.resolve();
    expect(pending.connect).toHaveBeenCalledOnce();
    teardown.resolve();
    await vi.waitFor(() => expect(pending.connect).toHaveBeenCalledTimes(2));

    pending.setAccounts([account('connected')]);
    connection.resolve();
    await Promise.all([first, retry]);
    expect(state.current.connected?.wallet).toBe(pending.wallet);
    expect(state.current).toMatchObject({ ready: true, connecting: null, error: null });
  });

  it('waits for an active wallet to disconnect before reconnecting it', async () => {
    const teardown = deferred<void>();
    const connected = wallet('Wallet', Promise.resolve(), teardown.promise);
    connected.setAccounts([account('connected')]);
    const { controller, state } = connectionController();

    await controller.connect(connected.wallet);
    const disconnect = controller.disconnect();
    const reconnect = controller.connect(connected.wallet);

    await Promise.resolve();
    expect(connected.connect).toHaveBeenCalledOnce();
    teardown.resolve();
    await disconnect;
    await reconnect;

    expect(connected.connect).toHaveBeenCalledTimes(2);
    expect(state.current.connected?.wallet).toBe(connected.wallet);
    expect(state.current).toMatchObject({ ready: true, connecting: null, error: null });
  });

  it('does not race teardown when switching from one wallet and immediately back', async () => {
    const teardown = deferred<void>();
    const first = wallet('First', Promise.resolve(), teardown.promise);
    const second = wallet('Second', Promise.resolve());
    first.setAccounts([account('first')]);
    second.setAccounts([account('second')]);
    const { controller, state } = connectionController();

    await controller.connect(first.wallet);
    const switchAway = controller.connect(second.wallet);
    const switchBack = controller.connect(first.wallet);

    await Promise.resolve();
    expect(first.connect).toHaveBeenCalledOnce();
    expect(second.connect).not.toHaveBeenCalled();

    teardown.resolve();
    await Promise.all([switchAway, switchBack]);

    expect(first.connect).toHaveBeenCalledTimes(2);
    expect(second.connect).not.toHaveBeenCalled();
    expect(state.current.connected?.wallet).toBe(first.wallet);
    expect(state.current).toMatchObject({ ready: true, connecting: null, error: null });
  });

  it('does not expose WalletConnect session details in connection errors', async () => {
    const pending = wallet(
      'WalletConnect',
      Promise.reject(
        new Error(
          'APKT005 relay failed for wc:pairing@2?topic=private-topic&projectId=b12827dfd1ff27064b91c710188bdbe4',
        ),
      ),
    );
    const { controller, state } = connectionController();

    await controller.connect(pending.wallet);

    expect(state.current.error).toBe(
      'WalletConnect could not open a secure session. Reopen the wallet and try again.',
    );
    expect(state.current.error).not.toContain('projectId');
    expect(state.current.error).not.toContain('private-topic');
  });
});

function connectionController() {
  const state = {
    current: {
      connected: null,
      ready: false,
      connecting: null,
      error: null,
    } as WalletConnectionState,
  };
  const controller = new WalletConnectionController(
    'transaction',
    'solana:mainnet',
    'Solana mainnet',
    (patch) => {
      state.current = { ...state.current, ...patch };
    },
  );
  return { controller, state };
}

function wallet(
  name: string,
  connectPromise: Promise<void>,
  disconnectPromise = Promise.resolve(),
) {
  let accounts: readonly WalletAccount[] = [];
  let change: ((value: { accounts?: readonly WalletAccount[] }) => void) | undefined;
  const connect = vi.fn(() => connectPromise);
  const disconnect = vi.fn(() => disconnectPromise);
  const events = vi.fn(
    (_event: 'change', listener: (value: { accounts?: readonly WalletAccount[] }) => void) => {
      change = listener;
      return vi.fn();
    },
  );
  const value = {
    version: '1.0.0',
    name,
    icon: 'data:image/svg+xml;base64,',
    chains: ['solana:mainnet'],
    get accounts() {
      return accounts;
    },
    features: {
      'standard:connect': { version: '1.0.0', connect },
      'standard:disconnect': { version: '1.0.0', disconnect },
      'standard:events': { version: '1.0.0', on: events },
      'solana:signTransaction': { version: '1.0.0', signTransaction: vi.fn() },
    },
  } as unknown as CompatibleWallet;
  return {
    wallet: value,
    connect,
    disconnect,
    events,
    setAccounts(value: readonly WalletAccount[]) {
      accounts = value;
    },
    change(value: readonly WalletAccount[]) {
      change?.({ accounts: value });
    },
  };
}

function deferred<T>() {
  let resolve!: (value: T | PromiseLike<T>) => void;
  let reject!: (reason?: unknown) => void;
  const promise = new Promise<T>((resolvePromise, rejectPromise) => {
    resolve = resolvePromise;
    reject = rejectPromise;
  });
  return { promise, resolve, reject };
}

function account(
  address: string,
  chains: string[] = ['solana:mainnet'],
  features: string[] = ['solana:signTransaction'],
): WalletAccount {
  return {
    address,
    publicKey: new Uint8Array(32),
    chains,
    features,
  } as WalletAccount;
}
