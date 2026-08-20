import { Connection, PublicKey } from "@solana/web3.js";

export declare const SAS_PROGRAM: PublicKey;
export declare const COVENANT_ISSUER: PublicKey;
export declare const SETTLEMENT_PROGRAM: PublicKey;
export declare const MAGIC_ROUTER: string;

export interface Route {
  identity: string;
  fqdn: string;
  countryCode?: string;
  baseFee?: number;
  blockTimeMs?: number;
}

export interface Resolution {
  verified: boolean;
  reason: string;
  attestation: PublicKey | null;
  trustedSigner?: boolean;
  notExpired?: boolean;
  status?: string;
  mrTd?: string;
  verifiedAt?: number;
  endpoint?: string;
  expiry?: number;
  signer?: string;
}

export declare function derivePdas(issuer?: PublicKey): { credential: PublicKey; schema: PublicKey };
export declare function deriveAttestationPda(validator: string | PublicKey, issuer?: PublicKey): PublicKey;
export declare function getRoutes(router?: string): Promise<Route[]>;
export declare function resolveVerifiedEr(
  connection: Connection,
  validator: string | PublicKey,
  opts?: { issuer?: PublicKey },
): Promise<Resolution>;
export declare function pickVerifiedEr(
  connection: Connection,
  opts?: { router?: string; issuer?: PublicKey },
): Promise<{ picked: (Route & { covenant: Resolution }) | null; routes: Array<Route & { covenant: Resolution }> }>;
export declare function foldProvenance(receiptHashes: Array<Uint8Array | string>): Uint8Array;
export declare function verifyProvenanceRoot(
  connection: Connection,
  creditsAccount: string | PublicKey,
  receiptHashes: Array<Uint8Array | string>,
): Promise<{ match: boolean; onChain: string; recomputed: string }>;
