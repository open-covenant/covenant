// Borsh encoders for the primitives the covenant IDLs use. Anchor instruction
// data is an 8-byte discriminator followed by the args in field order.
import { PublicKey } from '@solana/web3.js';

export type Bytes = Uint8Array;

type Numeric = bigint | number | string;

function toBigInt(value: Numeric, label: string): bigint {
  if (typeof value === 'bigint') return value;
  if (typeof value === 'number') {
    if (!Number.isInteger(value)) throw new Error(`${label} must be an integer, got ${value}`);
    return BigInt(value);
  }
  if (!/^-?\d+$/.test(value)) throw new Error(`${label} must be an integer string, got ${JSON.stringify(value)}`);
  return BigInt(value);
}

function int(byteLen: number, signed: boolean) {
  const bits = BigInt(byteLen * 8);
  const min = signed ? -(1n << (bits - 1n)) : 0n;
  const max = (signed ? 1n << (bits - 1n) : 1n << bits) - 1n;
  const label = `${signed ? 'i' : 'u'}${byteLen * 8}`;
  return (value: Numeric): Bytes => {
    const parsed = toBigInt(value, label);
    if (parsed < 0n && !signed) throw new Error(`unsigned integer cannot be negative: ${value}`);
    if (parsed < min || parsed > max) throw new Error(`value out of range for ${label}: ${value}`);
    let v = parsed < 0n ? parsed + (1n << bits) : parsed;
    const out = new Uint8Array(byteLen);
    for (let i = 0; i < byteLen; i++) {
      out[i] = Number(v & 0xffn);
      v >>= 8n;
    }
    return out;
  };
}

export const u16 = int(2, false);
export const u32 = int(4, false);
export const u64 = int(8, false);
export const i64 = int(8, true);

export const bool = (value: boolean): Bytes => Uint8Array.of(value ? 1 : 0);

export function hash32(hex: string): Bytes {
  // No 0x prefix, matching assertHash32 and the PDA seed encoder.
  if (!/^[0-9a-fA-F]{64}$/.test(hex)) {
    throw new Error(`expected a 32-byte hex string, got: ${hex}`);
  }
  const out = new Uint8Array(32);
  for (let i = 0; i < 32; i++) out[i] = parseInt(hex.slice(i * 2, i * 2 + 2), 16);
  return out;
}

export const pubkey = (address: string): Bytes => new PublicKey(address).toBytes();

export function option<T>(value: T | null | undefined, encode: (v: T) => Bytes): Bytes {
  return value === null || value === undefined ? Uint8Array.of(0) : concat([Uint8Array.of(1), encode(value)]);
}

export function concat(parts: Bytes[]): Bytes {
  const total = parts.reduce((n, p) => n + p.length, 0);
  const out = new Uint8Array(total);
  let offset = 0;
  for (const part of parts) {
    out.set(part, offset);
    offset += part.length;
  }
  return out;
}

// Sequential little-endian reader, the inverse of the encoders above. Account
// data is an 8-byte discriminator followed by the struct fields in order.
export class BorshReader {
  private pos = 0;
  constructor(private readonly data: Uint8Array) {}

  private take(n: number): Uint8Array {
    if (this.pos + n > this.data.length) throw new Error('borsh: unexpected end of buffer');
    const slice = this.data.subarray(this.pos, this.pos + n);
    this.pos += n;
    return slice;
  }

  private uint(byteLen: number): bigint {
    const b = this.take(byteLen);
    let v = 0n;
    for (let i = byteLen - 1; i >= 0; i--) v = (v << 8n) | BigInt(b[i]!);
    return v;
  }

  u8(): number {
    return this.take(1)[0]!;
  }
  u32(): number {
    return Number(this.uint(4));
  }
  u64(): bigint {
    return this.uint(8);
  }
  i64(): bigint {
    const v = this.uint(8);
    return v >= 1n << 63n ? v - (1n << 64n) : v;
  }
  bool(): boolean {
    return this.u8() !== 0;
  }
  pubkey(): string {
    return new PublicKey(this.take(32)).toBase58();
  }
  hash32(): string {
    return Buffer.from(this.take(32)).toString('hex');
  }
  discriminator(): Uint8Array {
    return Uint8Array.from(this.take(8));
  }
  remaining(): number {
    return this.data.length - this.pos;
  }
}
