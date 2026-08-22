'use client';

import { useCallback, useEffect, useMemo, useState } from 'react';
import { getWallets } from '@wallet-standard/app';
import type { Wallet, WalletAccount } from '@wallet-standard/base';
import type {
  SolanaSignMessageFeature,
  SolanaSignTransactionFeature,
} from '@solana/wallet-standard-features';
import type { StandardConnectFeature } from '@wallet-standard/features';

type CompatibleWallet = Wallet &
  Partial<StandardConnectFeature & SolanaSignMessageFeature & SolanaSignTransactionFeature>;

export type ConnectedWallet = {
  wallet: CompatibleWallet;
  account: WalletAccount;
};

export function useStandardWallet(requirement: 'message' | 'transaction') {
  const registry = useMemo(() => getWallets(), []);
  const [wallets, setWallets] = useState<readonly CompatibleWallet[]>([]);
  const [connected, setConnected] = useState<ConnectedWallet | null>(null);
  const [connecting, setConnecting] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);

  const refresh = useCallback(() => {
    setWallets(
      registry.get().filter((wallet): wallet is CompatibleWallet => {
        const canConnect = 'standard:connect' in wallet.features;
        const feature = requirement === 'message' ? 'solana:signMessage' : 'solana:signTransaction';
        return (
          canConnect &&
          feature in wallet.features &&
          wallet.chains.some((chain) => chain.startsWith('solana:'))
        );
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

  const connect = useCallback(async (wallet: CompatibleWallet) => {
    setConnecting(wallet.name);
    setError(null);
    try {
      const feature = wallet.features[
        'standard:connect'
      ] as StandardConnectFeature['standard:connect'];
      const result = await feature.connect();
      const account = result.accounts.find((candidate) =>
        candidate.chains.some((chain) => chain.startsWith('solana:')),
      );
      if (!account) throw new Error('This wallet did not expose a Solana account');
      const value = { wallet, account };
      setConnected(value);
      return value;
    } catch (cause) {
      const message = cause instanceof Error ? cause.message : 'Wallet connection was declined';
      setError(message);
      throw cause;
    } finally {
      setConnecting(null);
    }
  }, []);

  return { wallets, connected, connecting, error, connect };
}

export function bytesToBase64(value: Uint8Array): string {
  let binary = '';
  for (const byte of value) binary += String.fromCharCode(byte);
  return btoa(binary);
}
