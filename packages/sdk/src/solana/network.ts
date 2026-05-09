import {
  explorerHref,
  resolveCovenantNetwork,
  type ResolvedCovenantSolanaNetwork,
} from '@covenant/config/networks';
import { assertSolanaAddress } from './accounts.js';

export type CovenantSolanaNetworkConfig = ResolvedCovenantSolanaNetwork;

export function resolveSolanaNetwork(): CovenantSolanaNetworkConfig {
  const network = resolveCovenantNetwork();
  assertSolanaAddress(network.programId, 'program id');
  if (network.covntMint) assertSolanaAddress(network.covntMint, 'COVNT mint');
  return network;
}

export function solanaExplorerHref(kind: 'address' | 'tx', value: string): string {
  return explorerHref(kind, value, resolveSolanaNetwork());
}
