import { jwtVerify } from 'jose';
import { isSolanaAddress, type SolanaAddress } from '../solana/accounts.js';

export const SESSION_ISSUER = 'covenant.session';

export interface SessionPayload {
  address: SolanaAddress;
  expiresAt: number;
}

export function sessionSecret(raw = process.env.SESSION_SECRET): Uint8Array {
  if (!raw) {
    throw new Error('SESSION_SECRET is required');
  }
  return new TextEncoder().encode(raw);
}

export async function verifySessionJwt(
  token: string,
  secret: Uint8Array,
): Promise<SessionPayload | null> {
  try {
    const { payload } = await jwtVerify(token, secret, { issuer: SESSION_ISSUER });
    if (typeof payload.sub !== 'string' || typeof payload.exp !== 'number') {
      return null;
    }
    if (!isSolanaAddress(payload.sub)) return null;
    return {
      address: payload.sub,
      expiresAt: payload.exp,
    };
  } catch {
    return null;
  }
}
