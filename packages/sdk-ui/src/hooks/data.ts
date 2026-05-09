'use client';

import { useMutation, useQuery } from '@tanstack/react-query';
import {
  MOCK_CREDITS,
  MOCK_LEADERBOARD,
  type CreditSummary,
  type LeaderboardRow,
  type SolanaAddress,
} from '@covenant/sdk';

function resolveCredits(owner?: SolanaAddress) {
  if (!owner) return MOCK_CREDITS;
  return MOCK_CREDITS.filter((entry) => entry.owner === owner);
}

export function useCredits(owner?: SolanaAddress) {
  return useQuery<CreditSummary[]>({
    queryKey: ['covenant', 'credits', owner ?? 'all'],
    queryFn: async () => resolveCredits(owner),
    initialData: resolveCredits(owner),
  });
}

export function useBuyCredits() {
  return useMutation({
    mutationFn: async (input: {
      owner: SolanaAddress;
      amountCovnt: string;
    }) => input,
  });
}

export function useAllowedMints() {
  return useQuery({
    queryKey: ['covenant', 'allowed-mints'],
    queryFn: async () => [process.env.NEXT_PUBLIC_COVNT_MINT ?? ''],
    initialData: [process.env.NEXT_PUBLIC_COVNT_MINT ?? ''],
  });
}

export function useLeaderboard() {
  return useQuery<LeaderboardRow[]>({
    queryKey: ['covenant', 'leaderboard'],
    queryFn: async () => MOCK_LEADERBOARD,
    initialData: MOCK_LEADERBOARD,
  });
}
