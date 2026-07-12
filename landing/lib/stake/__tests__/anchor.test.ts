import { describe, expect, it } from "vitest";
import {
  anchorDiscriminator,
  concatBytes,
  i64le,
  readBool,
  readI64LE,
  readU16LE,
  readU32LE,
  readU64LE,
  readU128LE,
  u8,
  u16le,
  u32le,
  u64le,
  u128le,
} from "../anchor";

const bytes = (b: Uint8Array) => Array.from(b);

describe("anchorDiscriminator", () => {
  it("emits sha256('global:<method>') truncated to 8 bytes", () => {
    expect(bytes(anchorDiscriminator("create_position"))).toEqual([48, 215, 197, 153, 96, 203, 180, 133]);
    expect(bytes(anchorDiscriminator("claim"))).toEqual([62, 198, 214, 193, 213, 159, 108, 210]);
    expect(bytes(anchorDiscriminator("close_position"))).toEqual([123, 134, 81, 0, 49, 68, 98, 98]);
  });

  it("is exactly 8 bytes", () => {
    expect(anchorDiscriminator("claim")).toHaveLength(8);
  });

  it("namespaces by method so distinct instructions diverge", () => {
    expect(bytes(anchorDiscriminator("claim"))).not.toEqual(bytes(anchorDiscriminator("create_position")));
  });
});

describe("concatBytes", () => {
  it("joins parts in order at the right offsets", () => {
    const out = concatBytes(new Uint8Array([1, 2]), new Uint8Array([3]), new Uint8Array([4, 5, 6]));
    expect(bytes(out)).toEqual([1, 2, 3, 4, 5, 6]);
  });

  it("returns an empty array for no parts", () => {
    expect(bytes(concatBytes())).toEqual([]);
  });

  it("skips empty parts without shifting following bytes", () => {
    expect(bytes(concatBytes(new Uint8Array([7]), new Uint8Array([]), new Uint8Array([8])))).toEqual([7, 8]);
  });
});

describe("u8", () => {
  it("encodes a single byte across the unsigned range", () => {
    expect(bytes(u8(0))).toEqual([0]);
    expect(bytes(u8(255))).toEqual([255]);
  });

  it("masks to the low 8 bits", () => {
    expect(bytes(u8(256))).toEqual([0]);
    expect(bytes(u8(257))).toEqual([1]);
  });
});

describe("u16le", () => {
  it("writes little-endian byte order", () => {
    expect(bytes(u16le(0x0102))).toEqual([0x02, 0x01]);
  });

  it("covers zero and the max u16", () => {
    expect(bytes(u16le(0))).toEqual([0, 0]);
    expect(bytes(u16le(65535))).toEqual([255, 255]);
  });
});

describe("u32le", () => {
  it("writes little-endian byte order", () => {
    expect(bytes(u32le(0x01020304))).toEqual([0x04, 0x03, 0x02, 0x01]);
  });

  it("wraps negatives to unsigned and covers the max u32", () => {
    expect(bytes(u32le(-1))).toEqual([255, 255, 255, 255]);
    expect(bytes(u32le(0xffffffff))).toEqual([255, 255, 255, 255]);
  });
});

describe("u64le", () => {
  it("writes little-endian byte order", () => {
    expect(bytes(u64le(0x0102030405060708n))).toEqual([0x08, 0x07, 0x06, 0x05, 0x04, 0x03, 0x02, 0x01]);
  });

  it("covers zero and the max u64", () => {
    expect(bytes(u64le(0n))).toEqual([0, 0, 0, 0, 0, 0, 0, 0]);
    expect(bytes(u64le((1n << 64n) - 1n))).toEqual([255, 255, 255, 255, 255, 255, 255, 255]);
  });
});

describe("i64le", () => {
  it("writes -1 as two's-complement little-endian", () => {
    expect(bytes(i64le(-1n))).toEqual([255, 255, 255, 255, 255, 255, 255, 255]);
  });

  it("writes small positives little-endian", () => {
    expect(bytes(i64le(1n))).toEqual([1, 0, 0, 0, 0, 0, 0, 0]);
  });
});

describe("u128le", () => {
  it("splits into low then high little-endian 64-bit halves", () => {
    const v = (0x0102030405060708n << 64n) | 0x1112131415161718n;
    expect(bytes(u128le(v))).toEqual([
      0x18, 0x17, 0x16, 0x15, 0x14, 0x13, 0x12, 0x11,
      0x08, 0x07, 0x06, 0x05, 0x04, 0x03, 0x02, 0x01,
    ]);
  });

  it("covers zero and the max u128", () => {
    expect(bytes(u128le(0n))).toEqual(new Array(16).fill(0));
    expect(bytes(u128le((1n << 128n) - 1n))).toEqual(new Array(16).fill(255));
  });
});

describe("readU64LE", () => {
  it("reads little-endian from a fixed buffer", () => {
    expect(readU64LE(new Uint8Array([1, 0, 0, 0, 0, 0, 0, 0]), 0)).toBe(1n);
    expect(readU64LE(new Uint8Array([0, 1, 0, 0, 0, 0, 0, 0]), 0)).toBe(256n);
  });

  it("honors the offset", () => {
    expect(readU64LE(new Uint8Array([0xff, 1, 0, 0, 0, 0, 0, 0, 0]), 1)).toBe(1n);
  });

  it("round-trips with u64le", () => {
    const v = 0x0102030405060708n;
    expect(readU64LE(u64le(v), 0)).toBe(v);
  });
});

describe("readI64LE", () => {
  it("reads signed little-endian from a fixed buffer", () => {
    expect(readI64LE(new Uint8Array([255, 255, 255, 255, 255, 255, 255, 255]), 0)).toBe(-1n);
  });

  it("round-trips negatives with i64le", () => {
    expect(readI64LE(i64le(-123456789n), 0)).toBe(-123456789n);
  });
});

describe("readU32LE", () => {
  it("reads little-endian from a fixed buffer", () => {
    expect(readU32LE(new Uint8Array([1, 0, 0, 0]), 0)).toBe(1);
    expect(readU32LE(new Uint8Array([0, 0, 0, 1]), 0)).toBe(0x01000000);
  });

  it("honors the offset and round-trips", () => {
    const buf = concatBytes(new Uint8Array([9]), u32le(0xdeadbeef));
    expect(readU32LE(buf, 1)).toBe(0xdeadbeef);
  });
});

describe("readU16LE", () => {
  it("reads little-endian from a fixed buffer", () => {
    expect(readU16LE(new Uint8Array([2, 1]), 0)).toBe(0x0102);
  });

  it("honors the offset and round-trips", () => {
    const buf = concatBytes(new Uint8Array([9]), u16le(0xabcd));
    expect(readU16LE(buf, 1)).toBe(0xabcd);
  });
});

describe("readU128LE", () => {
  it("reconstructs (hi << 64) | lo from the two halves", () => {
    const v = (0x0102030405060708n << 64n) | 0x1112131415161718n;
    expect(readU128LE(u128le(v), 0)).toBe(v);
  });

  it("honors the offset", () => {
    const buf = concatBytes(new Uint8Array([0]), u128le(42n));
    expect(readU128LE(buf, 1)).toBe(42n);
  });
});

describe("readBool", () => {
  it("treats only the byte 1 as true", () => {
    expect(readBool(new Uint8Array([1]), 0)).toBe(true);
    expect(readBool(new Uint8Array([0]), 0)).toBe(false);
    expect(readBool(new Uint8Array([2]), 0)).toBe(false);
  });

  it("honors the offset", () => {
    expect(readBool(new Uint8Array([0, 1]), 1)).toBe(true);
  });
});
