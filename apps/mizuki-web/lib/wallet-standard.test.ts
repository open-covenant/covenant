import type { WalletAccount } from '@wallet-standard/base';
import { describe, expect, it, vi } from 'vitest';
import {
  observeWalletAccounts,
  selectSolanaAccount,
  type CompatibleWallet,
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
});

function account(address: string, chains: string[] = ['solana:mainnet']): WalletAccount {
  return {
    address,
    publicKey: new Uint8Array(32),
    chains,
    features: [],
  } as WalletAccount;
}
