'use client';

import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query';
import type { SolanaAddress } from '@covenant/sdk';
import { getJson } from '../http/json.js';

export interface Session {
  address: SolanaAddress;
  expiresAt: string;
}

export function useSession() {
  return useQuery({
    queryKey: ['covenant', 'session'],
    queryFn: async () => {
      const payload = await getJson<{ session: Session | null }>('/api/auth/me');
      return payload.session;
    },
  });
}

export function useSignOut() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: async () => getJson<{ ok: true }>('/api/auth/logout', { method: 'POST' }),
    onSuccess: async () => {
      await queryClient.invalidateQueries({ queryKey: ['covenant', 'session'] });
    },
  });
}
