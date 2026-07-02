import { describe, expect, it } from 'vitest';
import * as borsh from '../solana/borsh.js';

const bytes = (...b: number[]) => Uint8Array.from(b);

describe('borsh primitives', () => {
  it('encodes unsigned integers little-endian', () => {
    expect(borsh.u16(5000)).toEqual(bytes(0x88, 0x13));
    expect(borsh.u32(1)).toEqual(bytes(1, 0, 0, 0));
    expect(borsh.u64(1)).toEqual(bytes(1, 0, 0, 0, 0, 0, 0, 0));
    expect(borsh.u64('255')).toEqual(bytes(255, 0, 0, 0, 0, 0, 0, 0));
    expect(borsh.u64(2n ** 64n - 1n)).toEqual(new Uint8Array(8).fill(255));
  });

  it('encodes signed i64 in twos-complement', () => {
    expect(borsh.i64(1)).toEqual(bytes(1, 0, 0, 0, 0, 0, 0, 0));
    expect(borsh.i64(-1)).toEqual(new Uint8Array(8).fill(255));
    expect(borsh.i64(-2)).toEqual(bytes(254, 255, 255, 255, 255, 255, 255, 255));
    expect(borsh.i64(2n ** 63n - 1n)).toEqual(bytes(255, 255, 255, 255, 255, 255, 255, 127));
    expect(borsh.i64(-(2n ** 63n))).toEqual(bytes(0, 0, 0, 0, 0, 0, 0, 128));
  });

  it('rejects signed values outside the i64 range', () => {
    expect(() => borsh.i64(2n ** 63n)).toThrow(/out of range/);
    expect(() => borsh.i64(-(2n ** 63n) - 1n)).toThrow(/out of range/);
    expect(() => borsh.i64(2n ** 64n - 1n)).toThrow(/out of range/);
  });

  it('rejects non-integer numeric input', () => {
    expect(() => borsh.u64(1.5)).toThrow(/integer/);
    expect(() => borsh.u64('')).toThrow(/integer string/);
    expect(() => borsh.u64('0x10')).toThrow(/integer string/);
    expect(() => borsh.u64('abc')).toThrow(/integer string/);
  });

  it('rejects negative unsigned and out-of-range values', () => {
    expect(() => borsh.u64(-1)).toThrow(/negative/);
    expect(() => borsh.u64(2n ** 64n)).toThrow(/out of range/);
    expect(() => borsh.u16(65536)).toThrow(/out of range/);
  });

  it('encodes bool as one byte', () => {
    expect(borsh.bool(true)).toEqual(bytes(1));
    expect(borsh.bool(false)).toEqual(bytes(0));
  });

  it('encodes a 32-byte hash from hex and rejects bad input', () => {
    expect(borsh.hash32('00'.repeat(32))).toEqual(new Uint8Array(32));
    expect(borsh.hash32('ff'.repeat(32))).toEqual(new Uint8Array(32).fill(255));
    expect(borsh.hash32('0x' + 'ab'.repeat(32))[0]).toBe(0xab);
    expect(() => borsh.hash32('ab')).toThrow(/32-byte hex/);
    expect(() => borsh.hash32('zz'.repeat(32))).toThrow(/32-byte hex/);
  });

  it('encodes a pubkey as its 32 raw bytes', () => {
    expect(borsh.pubkey('11111111111111111111111111111111')).toEqual(new Uint8Array(32));
  });

  it('encodes option as a presence tag plus the inner value', () => {
    expect(borsh.option(null, borsh.u64)).toEqual(bytes(0));
    expect(borsh.option(undefined, borsh.u64)).toEqual(bytes(0));
    expect(borsh.option('1', borsh.u64)).toEqual(bytes(1, 1, 0, 0, 0, 0, 0, 0, 0));
  });

  it('concatenates chunks in order', () => {
    expect(borsh.concat([bytes(1), bytes(2, 3), bytes(4)])).toEqual(bytes(1, 2, 3, 4));
  });
});
