import { Keypair, PublicKey, Transaction, VersionedTransaction } from '@solana/web3.js';

// A minimal signer the SDK drives in Node or the browser. This is the same shape
// a @solana/wallet-adapter wallet exposes, so an adapter satisfies it directly.
export interface CovenantSigner {
  publicKey: PublicKey;
  signTransaction<T extends Transaction | VersionedTransaction>(tx: T): Promise<T>;
  signAllTransactions?<T extends Transaction | VersionedTransaction>(txs: T[]): Promise<T[]>;
}

function sign<T extends Transaction | VersionedTransaction>(tx: T, keypair: Keypair): T {
  if (tx instanceof VersionedTransaction) tx.sign([keypair]);
  else tx.partialSign(keypair);
  return tx;
}

// Wrap a local Keypair for server-side signing.
export function keypairSigner(keypair: Keypair): CovenantSigner {
  return {
    publicKey: keypair.publicKey,
    async signTransaction<T extends Transaction | VersionedTransaction>(tx: T): Promise<T> {
      return sign(tx, keypair);
    },
    async signAllTransactions<T extends Transaction | VersionedTransaction>(txs: T[]): Promise<T[]> {
      return txs.map((tx) => sign(tx, keypair));
    },
  };
}

// Structural type for a @solana/wallet-adapter wallet, so the SDK adapts one
// without taking a hard dependency on the wallet-adapter packages. A real
// adapter (Phantom, Solflare, Backpack, ...) matches this shape.
export interface WalletAdapterLike {
  publicKey: PublicKey | null;
  signTransaction?<T extends Transaction | VersionedTransaction>(tx: T): Promise<T>;
  signAllTransactions?<T extends Transaction | VersionedTransaction>(txs: T[]): Promise<T[]>;
}

export function walletAdapterSigner(adapter: WalletAdapterLike): CovenantSigner {
  if (!adapter.publicKey) throw new Error('wallet is not connected (publicKey is null)');
  if (!adapter.signTransaction) throw new Error('wallet does not support signTransaction');
  // Read publicKey live rather than snapshotting it, so a mid-session account
  // switch or reconnect can't leave the fee payer stale against a fresh wallet.
  const signer: CovenantSigner = {
    get publicKey(): PublicKey {
      if (!adapter.publicKey) throw new Error('wallet disconnected (publicKey is null)');
      return adapter.publicKey;
    },
    signTransaction: adapter.signTransaction.bind(adapter),
  };
  if (adapter.signAllTransactions) {
    signer.signAllTransactions = adapter.signAllTransactions.bind(adapter);
  }
  return signer;
}
