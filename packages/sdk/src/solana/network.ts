import {
  explorerHref,
  resolveCovenantNetwork,
  type NetworkOverrides,
  type ResolvedCovenantSolanaNetwork,
} from '../config.js';
import { assertSolanaAddress } from './accounts.js';

export type CovenantSolanaNetworkConfig = ResolvedCovenantSolanaNetwork;

export function resolveSolanaNetwork(overrides: NetworkOverrides = {}): CovenantSolanaNetworkConfig {
  const network = resolveCovenantNetwork(undefined, overrides);
  assertSolanaAddress(network.programId, 'program id');
  if (network.covntMint) assertSolanaAddress(network.covntMint, 'CVNT mint');
  return network;
}

export function solanaExplorerHref(kind: 'address' | 'tx', value: string): string {
  return explorerHref(kind, value, resolveSolanaNetwork());
}
