'use client';

import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
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

export function useStandardWallet(requirement: 'message' | 'transaction') {
  const registry = useMemo(() => getWallets(), []);
  const [wallets, setWallets] = useState<readonly CompatibleWallet[]>([]);
  const [connected, setConnected] = useState<ConnectedWallet | null>(null);
  const [ready, setReady] = useState(false);
  const [connecting, setConnecting] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const stopObserving = useRef<(() => void) | null>(null);
  const connectionGeneration = useRef(0);

  const refresh = useCallback(() => {
    setWallets(
      registry.get().filter((wallet): wallet is CompatibleWallet => {
        return supportsWallet(wallet, requirement);
      }),
    );
  }, [registry, requirement]);

  useEffect(() => {
    refresh();
    const offRegister = registry.on('register', refresh);
    const offUnregister = registry.on('unregister', refresh);
    return () => {
      offRegister();
      offUnregister();
    };
  }, [refresh, registry]);

  useEffect(
    () => () => {
      connectionGeneration.current += 1;
      stopObserving.current?.();
    },
    [],
  );

  const connect = useCallback(
    async (wallet: CompatibleWallet) => {
      const generation = connectionGeneration.current + 1;
      connectionGeneration.current = generation;
      stopObserving.current?.();
      stopObserving.current = null;
      setConnected(null);
      setReady(false);
      setConnecting(wallet.name);
      setError(null);
      try {
        const feature = wallet.features[
          'standard:connect'
        ] as StandardConnectFeature['standard:connect'];
        const result = await feature.connect();
        const account = selectSolanaAccount(result.accounts);
        if (!account) throw new Error('This wallet did not expose a Solana account');
        if ('standard:events' in wallet.features) {
          stopObserving.current = observeWalletAccounts(wallet, (next) => {
            if (connectionGeneration.current !== generation) return;
            if (!next) {
              setReady(false);
              setConnected(null);
              return;
            }
            setConnected({ wallet, account: next });
            setReady(true);
          });
        } else if (requirement === 'transaction') {
          throw new Error('This wallet cannot report account or disconnect changes');
        }
        const value = { wallet, account };
        setConnected(value);
        setReady(true);
        return value;
      } catch (cause) {
        stopObserving.current?.();
        stopObserving.current = null;
        const message = cause instanceof Error ? cause.message : 'Wallet connection was declined';
        setError(message);
        throw cause;
      } finally {
        setConnecting(null);
      }
    },
    [requirement],
  );

  const disconnect = useCallback(async () => {
    const wallet = connected?.wallet;
    connectionGeneration.current += 1;
    stopObserving.current?.();
    stopObserving.current = null;
    setReady(false);
    setConnected(null);
    setError(null);
    if (!wallet || !('standard:disconnect' in wallet.features)) return;
    try {
      const feature = wallet.features[
        'standard:disconnect'
      ] as StandardDisconnectFeature['standard:disconnect'];
      await feature.disconnect();
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : 'Wallet disconnect failed');
    }
  }, [connected]);

  return { wallets, connected, ready, connecting, error, connect, disconnect };
}

export function observeWalletAccounts(
  wallet: CompatibleWallet,
  update: (account: WalletAccount | null) => void,
): () => void {
  if (!('standard:events' in wallet.features)) {
    throw new Error('This wallet cannot report account or disconnect changes');
  }
  const events = wallet.features['standard:events'] as StandardEventsFeature['standard:events'];
  return events.on('change', ({ accounts }) => {
    if (accounts) update(selectSolanaAccount(accounts));
  });
}

export function selectSolanaAccount(accounts: readonly WalletAccount[]): WalletAccount | null {
  return (
    accounts.find((candidate) => candidate.chains.some((chain) => chain.startsWith('solana:'))) ??
    null
  );
}

function supportsWallet(wallet: Wallet, requirement: 'message' | 'transaction') {
  const feature = requirement === 'message' ? 'solana:signMessage' : 'solana:signTransaction';
  return (
    'standard:connect' in wallet.features &&
    (requirement === 'message' || 'standard:events' in wallet.features) &&
    feature in wallet.features &&
    wallet.chains.some((chain) => chain.startsWith('solana:'))
  );
}

export function bytesToBase64(value: Uint8Array): string {
  let binary = '';
  for (const byte of value) binary += String.fromCharCode(byte);
  return btoa(binary);
}
