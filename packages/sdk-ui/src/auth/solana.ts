'use client';

import { useMutation, useQueryClient } from '@tanstack/react-query';
import { isSolanaAddress, type SolanaAddress } from '@covenant/sdk';
import { getJson } from '../http/json.js';

declare global {
  interface Window {
    solana?: {
      connect(): Promise<{ publicKey: { toString(): string } }>;
      signMessage?(message: Uint8Array, encoding?: string): Promise<{ signature: Uint8Array }>;
    };
  }
}

async function connectWallet(): Promise<SolanaAddress> {
  if (!window.solana) {
    throw new Error('No Solana wallet detected');
  }
  const response = await window.solana.connect();
  const address = response.publicKey.toString();
  if (!isSolanaAddress(address)) {
    throw new Error('Wallet returned an invalid Solana address');
  }
  return address;
}

async function requestNonce(address: SolanaAddress): Promise<{ message: string }> {
  const payload = await getJson<{ message?: string }>('/api/auth/nonce', {
    method: 'POST',
    body: JSON.stringify({ address }),
  });
  if (!payload.message) throw new Error('Failed to issue wallet nonce');
  return { message: payload.message };
}

async function signMessage(message: string) {
  if (!window.solana?.signMessage) {
    throw new Error('Wallet does not support message signing');
  }
  const encoded = new TextEncoder().encode(message);
  const signed = await window.solana.signMessage(encoded, 'utf8');
  return Array.from(signed.signature);
}

async function verifySignature(address: SolanaAddress, message: string, signature: number[]) {
  await getJson('/api/auth/verify', {
    method: 'POST',
    body: JSON.stringify({ address, message, signature }),
  });
}

export function useSolanaSignIn() {
  const queryClient = useQueryClient();

  return useMutation({
    mutationFn: async () => {
      const address = await connectWallet();
      const { message } = await requestNonce(address);
      const signature = await signMessage(message);
      await verifySignature(address, message, signature);
      return address;
    },
    onSuccess: async () => {
      await queryClient.invalidateQueries({ queryKey: ['covenant', 'session'] });
    },
  });
}
