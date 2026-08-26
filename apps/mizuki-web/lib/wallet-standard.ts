'use client';

import { useCallback, useEffect, useMemo, useState } from 'react';
import { getWallets } from '@wallet-standard/app';
import type { Wallet, WalletAccount } from '@wallet-standard/base';
import type {
  SolanaSignMessageFeature,
  SolanaSignTransactionFeature,
} from '@solana/wallet-standard-features';
import type {
  StandardConnectFeature,
  StandardDisconnectFeature,
  StandardEventsFeature,
} from '@wallet-standard/features';
import { connectWithWalletConnect, signWalletConnectTransactions } from './reown-appkit';

export type CompatibleWallet = Wallet &
  Partial<
    StandardConnectFeature &
      StandardDisconnectFeature &
      StandardEventsFeature &
      SolanaSignMessageFeature &
      SolanaSignTransactionFeature
  >;

export type ConnectedWallet = {
  wallet: CompatibleWallet;
  account: WalletAccount;
};

export type PaymentWalletNetwork = {
  chain: 'solana:mainnet' | 'solana:devnet';
  label: 'Solana mainnet' | 'Solana devnet';
};

export type WalletConnectionState = {
  connected: ConnectedWallet | null;
  ready: boolean;
  connecting: string | null;
  error: string | null;
};

const initialConnectionState: WalletConnectionState = {
  connected: null,
  ready: false,
  connecting: null,
  error: null,
};

const walletConnectAdapters = new WeakMap<CompatibleWallet, CompatibleWallet>();

export function paymentWalletNetwork(): PaymentWalletNetwork {
  return process.env.NEXT_PUBLIC_SOLANA_NETWORK === 'solana-devnet'
    ? { chain: 'solana:devnet', label: 'Solana devnet' }
    : { chain: 'solana:mainnet', label: 'Solana mainnet' };
}

export function useStandardWallet(requirement: 'message' | 'transaction') {
  const registry = useMemo(() => getWallets(), []);
  const paymentNetwork = paymentWalletNetwork();
  const requiredChain = requirement === 'transaction' ? paymentNetwork.chain : undefined;
  const [wallets, setWallets] = useState<readonly CompatibleWallet[]>([]);
  const [connection, setConnection] = useState<WalletConnectionState>(initialConnectionState);
  const publishConnection = useCallback(
    (patch: Partial<WalletConnectionState>) =>
      setConnection((current) => ({ ...current, ...patch })),
    [],
  );
  const controller = useMemo(
    () =>
      new WalletConnectionController(
        requirement,
        requiredChain,
        paymentNetwork.label,
        publishConnection,
      ),
    [paymentNetwork.label, publishConnection, requiredChain, requirement],
  );

  const refresh = useCallback(() => {
    setWallets(
      registry
        .get()
        .map(adaptWalletConnect)
        .filter((wallet): wallet is CompatibleWallet =>
          supportsWallet(wallet, requirement, requiredChain),
        ),
    );
  }, [registry, requirement, requiredChain]);

  useEffect(() => {
    refresh();
    const offRegister = registry.on('register', refresh);
    const offUnregister = registry.on('unregister', refresh);
    return () => {
      offRegister();
      offUnregister();
    };
  }, [refresh, registry]);

  useEffect(() => () => controller.dispose(), [controller]);

  const connect = useCallback(
    (wallet: CompatibleWallet) => {
      if (wallet.name !== 'WalletConnect') return controller.connect(wallet);
      return connectWithWalletConnect(
        () => controller.connect(wallet),
        () => controller.cancelPending(),
        (message) => controller.reportError(message),
      );
    },
    [controller],
  );

  const disconnect = useCallback(() => controller.disconnect(), [controller]);

  return { wallets, ...connection, connect, disconnect };
}

export class WalletConnectionController {
  private generation = 0;
  private activeWallet: CompatibleWallet | null = null;
  private pendingWallet: CompatibleWallet | null = null;
  private connected: ConnectedWallet | null = null;
  private stopObserving: (() => void) | null = null;
  private disconnects = new WeakMap<CompatibleWallet, Promise<void>>();

  constructor(
    private readonly requirement: 'message' | 'transaction',
    private readonly requiredChain: PaymentWalletNetwork['chain'] | undefined,
    private readonly networkLabel: PaymentWalletNetwork['label'],
    private readonly publish: (patch: Partial<WalletConnectionState>) => void,
  ) {}

  connect(wallet: CompatibleWallet): Promise<ConnectedWallet | null> {
    const generation = this.nextGeneration();
    const previousWallet = this.activeWallet;
    this.clearConnection();
    this.pendingWallet = wallet;
    this.publish({ connected: null, ready: false, connecting: wallet.name, error: null });
    return (async () => {
      if (previousWallet) await this.startDisconnect(previousWallet);
      await this.waitForDisconnect(wallet);
      if (!this.current(generation)) return null;
      return this.connectCurrent(wallet, generation);
    })();
  }

  disconnect(): Promise<void> {
    const generation = this.nextGeneration();
    const wallet = this.activeWallet;
    this.clearConnection();
    this.publish({ connected: null, ready: false, connecting: null, error: null });
    if (!wallet) return Promise.resolve();
    return (async () => {
      try {
        await this.startDisconnect(wallet, true);
      } catch (cause) {
        if (!this.current(generation)) return;
        this.publish({ error: walletError(cause, 'Wallet disconnect failed') });
      }
    })();
  }

  cancelPending(): void {
    const wallet = this.pendingWallet;
    if (!wallet) return;
    this.nextGeneration();
    this.clearConnection();
    this.publish({ connected: null, ready: false, connecting: null, error: null });
    void this.startDisconnect(wallet);
  }

  reportError(error: string): void {
    this.publish({ connecting: null, error });
  }

  dispose(): void {
    this.nextGeneration();
    this.clearConnection();
  }

  private async connectCurrent(
    wallet: CompatibleWallet,
    generation: number,
  ): Promise<ConnectedWallet | null> {
    let unsubscribe: (() => void) | null = null;
    try {
      const feature = wallet.features['standard:connect'] as
        | StandardConnectFeature['standard:connect']
        | undefined;
      if (!feature) throw new Error('This wallet cannot open a compatible connection');
      await feature.connect();
      if (!this.current(generation)) {
        await this.disconnectSuperseded(wallet);
        return null;
      }

      if ('standard:events' in wallet.features) {
        unsubscribe = observeWalletAccounts(
          wallet,
          (account) => this.accountChanged(wallet, account, generation),
          this.requiredChain,
          this.requirement,
        );
        if (!this.current(generation)) {
          unsubscribe();
          await this.disconnectSuperseded(wallet);
          return null;
        }
        this.stopObserving = unsubscribe;
      } else if (this.requirement === 'transaction') {
        throw new Error('This wallet cannot report account or disconnect changes');
      }

      const account = selectSolanaAccount(wallet.accounts, this.requiredChain, this.requirement);
      if (!account) {
        throw new Error(
          this.requiredChain
            ? `This wallet did not expose a compatible ${this.networkLabel} account`
            : 'This wallet did not expose a compatible Solana account',
        );
      }
      if (!this.current(generation)) {
        unsubscribe?.();
        await this.disconnectSuperseded(wallet);
        return null;
      }
      const connected = { wallet, account };
      this.pendingWallet = null;
      this.activeWallet = wallet;
      this.connected = connected;
      this.publish({ connected, ready: true, error: null });
      return connected;
    } catch (cause) {
      if (!this.current(generation)) return null;
      unsubscribe?.();
      if (this.stopObserving === unsubscribe) this.stopObserving = null;
      await this.startDisconnect(wallet);
      if (!this.current(generation)) return null;
      this.activeWallet = null;
      this.connected = null;
      this.publish({
        connected: null,
        ready: false,
        error: walletError(cause, 'Wallet connection was declined'),
      });
      return null;
    } finally {
      if (this.current(generation)) {
        this.pendingWallet = null;
        this.publish({ connecting: null });
      }
    }
  }

  private accountChanged(
    wallet: CompatibleWallet,
    account: WalletAccount | null,
    generation: number,
  ): void {
    if (!this.current(generation)) return;
    if (!account) {
      this.connected = null;
      this.publish({ connected: null, ready: false });
      return;
    }
    const connected = { wallet, account };
    this.connected = connected;
    this.publish({ connected, ready: true, error: null });
  }

  private async disconnectSuperseded(wallet: CompatibleWallet): Promise<void> {
    if (wallet === this.activeWallet || wallet === this.pendingWallet) return;
    await this.startDisconnect(wallet);
  }

  private async waitForDisconnect(wallet: CompatibleWallet): Promise<void> {
    try {
      await this.disconnects.get(wallet);
    } catch {
      // The next connection still gets a chance to restore the wallet session.
    }
  }

  private startDisconnect(wallet: CompatibleWallet, strict = false): Promise<void> {
    const existing = this.disconnects.get(wallet);
    if (existing) return existing;
    const operation = disconnectWallet(wallet, strict).finally(() => {
      if (this.disconnects.get(wallet) === operation) this.disconnects.delete(wallet);
    });
    this.disconnects.set(wallet, operation);
    return operation;
  }

  private current(generation: number): boolean {
    return this.generation === generation;
  }

  private nextGeneration(): number {
    this.generation += 1;
    return this.generation;
  }

  private clearConnection(): void {
    this.stopObserving?.();
    this.stopObserving = null;
    this.activeWallet = null;
    this.pendingWallet = null;
    this.connected = null;
  }
}

export function adaptWalletConnect(wallet: Wallet): CompatibleWallet {
  const compatible = wallet as CompatibleWallet;
  if (wallet.name !== 'WalletConnect' || !('solana:signTransaction' in wallet.features)) {
    return compatible;
  }
  const cached = walletConnectAdapters.get(compatible);
  if (cached) return cached;

  const feature = wallet.features[
    'solana:signTransaction'
  ] as SolanaSignTransactionFeature['solana:signTransaction'];
  const features = {
    ...wallet.features,
    'solana:signTransaction': {
      ...feature,
      supportedTransactionVersions: ['legacy', 0] as const,
      signTransaction: signWalletConnectTransactions,
    },
  };
  const adapter: CompatibleWallet = {
    version: wallet.version,
    name: wallet.name,
    icon: wallet.icon,
    chains: wallet.chains,
    features,
    get accounts() {
      return wallet.accounts;
    },
  };
  walletConnectAdapters.set(compatible, adapter);
  return adapter;
}

async function disconnectWallet(wallet: CompatibleWallet, strict = false): Promise<void> {
  const feature = wallet.features['standard:disconnect'] as
    | StandardDisconnectFeature['standard:disconnect']
    | undefined;
  if (!feature) return;
  try {
    await feature.disconnect();
  } catch (cause) {
    if (strict) throw cause;
  }
}

function walletError(cause: unknown, fallback: string): string {
  if (!(cause instanceof Error) || !cause.message) return fallback;
  if (/compatible solana (mainnet|devnet) account|not available on solana/i.test(cause.message)) {
    return 'This wallet is not connected to the required Solana network. Switch networks or use another wallet.';
  }
  if (/reject|declin|cancel|closed/i.test(cause.message)) {
    return 'Wallet connection was cancelled. No payment or job was created.';
  }
  if (/walletconnect|relay|pairing|topic|wc:|APKT\d+/i.test(cause.message)) {
    return 'WalletConnect could not open a secure session. Reopen the wallet and try again.';
  }
  return fallback;
}

export function observeWalletAccounts(
  wallet: CompatibleWallet,
  update: (account: WalletAccount | null) => void,
  requiredChain?: PaymentWalletNetwork['chain'],
  requirement?: 'message' | 'transaction',
): () => void {
  if (!('standard:events' in wallet.features)) {
    throw new Error('This wallet cannot report account or disconnect changes');
  }
  const events = wallet.features['standard:events'] as StandardEventsFeature['standard:events'];
  return events.on('change', ({ accounts }) => {
    if (accounts) update(selectSolanaAccount(accounts, requiredChain, requirement));
  });
}

export function selectSolanaAccount(
  accounts: readonly WalletAccount[],
  requiredChain?: PaymentWalletNetwork['chain'],
  requirement?: 'message' | 'transaction',
): WalletAccount | null {
  const requiredFeature =
    requirement === 'message'
      ? 'solana:signMessage'
      : requirement === 'transaction'
        ? 'solana:signTransaction'
        : undefined;
  return (
    accounts.find(
      (candidate) =>
        (requiredChain
          ? candidate.chains.includes(requiredChain)
          : candidate.chains.some((chain) => chain.startsWith('solana:'))) &&
        (!requiredFeature || candidate.features.includes(requiredFeature)),
    ) ?? null
  );
}

function supportsWallet(
  wallet: Wallet,
  requirement: 'message' | 'transaction',
  requiredChain?: PaymentWalletNetwork['chain'],
) {
  const feature = requirement === 'message' ? 'solana:signMessage' : 'solana:signTransaction';
  return (
    'standard:connect' in wallet.features &&
    (requirement === 'message' || 'standard:events' in wallet.features) &&
    feature in wallet.features &&
    (requiredChain
      ? wallet.chains.includes(requiredChain)
      : wallet.chains.some((chain) => chain.startsWith('solana:')))
  );
}

export function bytesToBase64(value: Uint8Array): string {
  let binary = '';
  for (const byte of value) binary += String.fromCharCode(byte);
  return btoa(binary);
}
