import { sha256 } from "@noble/hashes/sha2";

export function anchorDiscriminator(method: string): Uint8Array {
  const hash = sha256(new TextEncoder().encode(`global:${method}`));
  return hash.slice(0, 8);
}

export function concatBytes(...parts: Uint8Array[]): Uint8Array {
  const len = parts.reduce((n, p) => n + p.length, 0);
  const out = new Uint8Array(len);
  let i = 0;
  for (const p of parts) {
    out.set(p, i);
    i += p.length;
  }
  return out;
}

export function u8(v: number): Uint8Array {
  return new Uint8Array([v & 0xff]);
}

export function u16le(v: number): Uint8Array {
  const b = new Uint8Array(2);
  new DataView(b.buffer).setUint16(0, v, true);
  return b;
}

export function u32le(v: number): Uint8Array {
  const b = new Uint8Array(4);
  new DataView(b.buffer).setUint32(0, v >>> 0, true);
  return b;
}

export function u64le(v: bigint): Uint8Array {
  const b = new Uint8Array(8);
  new DataView(b.buffer).setBigUint64(0, v, true);
  return b;
}

export function i64le(v: bigint): Uint8Array {
  const b = new Uint8Array(8);
  new DataView(b.buffer).setBigInt64(0, v, true);
  return b;
}

export function u128le(v: bigint): Uint8Array {
  const b = new Uint8Array(16);
  const view = new DataView(b.buffer);
  view.setBigUint64(0, v & 0xffffffffffffffffn, true);
  view.setBigUint64(8, v >> 64n, true);
  return b;
}

export function readU64LE(buf: Uint8Array, offset: number): bigint {
  return new DataView(buf.buffer, buf.byteOffset, buf.byteLength).getBigUint64(
    offset,
    true,
  );
}

export function readI64LE(buf: Uint8Array, offset: number): bigint {
  return new DataView(buf.buffer, buf.byteOffset, buf.byteLength).getBigInt64(
    offset,
    true,
  );
}

export function readU32LE(buf: Uint8Array, offset: number): number {
  return new DataView(buf.buffer, buf.byteOffset, buf.byteLength).getUint32(
    offset,
    true,
  );
}

export function readU16LE(buf: Uint8Array, offset: number): number {
  return new DataView(buf.buffer, buf.byteOffset, buf.byteLength).getUint16(
    offset,
    true,
  );
}

export function readU128LE(buf: Uint8Array, offset: number): bigint {
  const view = new DataView(buf.buffer, buf.byteOffset, buf.byteLength);
  const lo = view.getBigUint64(offset, true);
  const hi = view.getBigUint64(offset + 8, true);
  return (hi << 64n) | lo;
}

export function readBool(buf: Uint8Array, offset: number): boolean {
  return buf[offset] === 1;
}
