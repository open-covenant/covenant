import { describe, it, expect } from "vitest";
import {
  ANCHOR_DISCRIMINATOR,
  buildBatchId,
  buildCountLe,
  buildInstructionData,
  assertRootLength,
} from "./anchor-lib.mjs";

// Constants precomputed with an independent node:crypto fold (not via
// anchor-lib) and frozen, so any drift in the batch_id input string, field
// order, or endianness breaks the assertion instead of passing a relational
// check. Fixtures: sha="abc123", root = "deadbeef" x8 (32 bytes), count=1.
const SHA = "abc123";
const ROOT = "deadbeef".repeat(8);
const DISC_HEX = "5acf5b79393db081";
const BATCH_ID_HEX = "57956c1439d12f520a254ba1a78af10eb9a7bba5f777329754bab7988fd0919a";
const DATA_HEX =
  "5acf5b79393db081" +
  "57956c1439d12f520a254ba1a78af10eb9a7bba5f777329754bab7988fd0919a" +
  "deadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeef" +
  "01000000";

describe("ANCHOR_DISCRIMINATOR", () => {
  it("pins the 8-byte anchor_receipt_batch global discriminator", () => {
    expect(ANCHOR_DISCRIMINATOR.toString("hex")).toBe(DISC_HEX);
    expect(ANCHOR_DISCRIMINATOR.length).toBe(8);
  });
});

describe("buildBatchId", () => {
  it("pins sha256 of the canonical '<sha>:<root>' input", () => {
    expect(buildBatchId(SHA, ROOT).toString("hex")).toBe(BATCH_ID_HEX);
  });

  it("depends on argument order (root-first would derive a different PDA)", () => {
    expect(buildBatchId(ROOT, SHA).toString("hex")).not.toBe(BATCH_ID_HEX);
  });
});

describe("buildCountLe", () => {
  it("pins little-endian u32 encoding", () => {
    expect(buildCountLe(1).toString("hex")).toBe("01000000");
    expect(buildCountLe(256).toString("hex")).toBe("00010000");
    expect(buildCountLe(1).length).toBe(4);
  });
});

describe("buildInstructionData", () => {
  it("pins the exact 76-byte layout discriminator||batch_id||merkle_root||count_le", () => {
    const data = buildInstructionData(SHA, ROOT, 1);
    expect(data.toString("hex")).toBe(DATA_HEX);
    expect(data.length).toBe(76);
  });

  it("places each field at its fixed offset", () => {
    const data = buildInstructionData(SHA, ROOT, 1);
    expect(data.subarray(0, 8).toString("hex")).toBe(DISC_HEX);
    expect(data.subarray(8, 40).toString("hex")).toBe(BATCH_ID_HEX);
    expect(data.subarray(40, 72).toString("hex")).toBe(ROOT);
    expect(data.subarray(72, 76).toString("hex")).toBe("01000000");
  });

  it("rejects a root that is not 32 bytes of hex", () => {
    expect(() => buildInstructionData(SHA, "deadbeef".repeat(7) + "de", 1)).toThrow(); // 63 hex chars
    expect(() => buildInstructionData(SHA, "zz".repeat(32), 1)).toThrow(); // non-hex -> 0 bytes
    expect(() => buildInstructionData(SHA, ROOT, 1)).not.toThrow();
  });
});

describe("assertRootLength", () => {
  it("passes exactly 32 bytes of hex and rejects everything else", () => {
    expect(() => assertRootLength(ROOT)).not.toThrow();
    expect(() => assertRootLength("ab")).toThrow();
    expect(() => assertRootLength("00".repeat(31))).toThrow();
    expect(() => assertRootLength("00".repeat(33))).toThrow();
  });
});
