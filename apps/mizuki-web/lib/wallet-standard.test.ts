import type { WalletAccount } from '@wallet-standard/base';
import { describe, expect, it, vi } from 'vitest';
import {
  observeWalletAccounts,
  paymentWalletNetwork,
  selectSolanaAccount,
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

  it('serializes overlapping connections and ignores stale completion state', async () => {
    const firstConnect = deferred<void>();
    const secondConnect = deferred<void>();
    const first = wallet('First', firstConnect.promise);
    const second = wallet('Second', secondConnect.promise);
    const { controller, state } = connectionController();

    const firstOperation = controller.connect(first.wallet);
    await Promise.resolve();
    expect(first.connect).toHaveBeenCalledOnce();
    const secondOperation = controller.connect(second.wallet);
    expect(second.connect).not.toHaveBeenCalled();

    first.setAccounts([account('first')]);
    firstConnect.resolve();
    await firstOperation;
    expect(first.disconnect).toHaveBeenCalledOnce();
    await vi.waitFor(() => expect(second.connect).toHaveBeenCalledOnce());
    expect(state.current).toMatchObject({ connected: null, connecting: 'Second', error: null });

    second.setAccounts([account('second')]);
    secondConnect.resolve();
    await secondOperation;
    expect(state.current.connected?.wallet).toBe(second.wallet);
    expect(state.current.connected?.account.address).toBe('second');
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

function account(address: string, chains: string[] = ['solana:mainnet']): WalletAccount {
  return {
    address,
    publicKey: new Uint8Array(32),
    chains,
    features: [],
  } as WalletAccount;
}
