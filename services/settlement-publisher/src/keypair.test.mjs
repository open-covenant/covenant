import { describe, it, expect, beforeEach, afterEach } from "vitest";
import { parseOperatorKeypairBytes } from "./keypair.mjs";

const ENV = "COVENANT_OPERATOR_KEYPAIR_JSON";

beforeEach(() => {
  delete process.env[ENV];
});

afterEach(() => {
  delete process.env[ENV];
});

const bytes = (n) => JSON.stringify(Array.from({ length: n }, () => 0));

describe("parseOperatorKeypairBytes", () => {
  it("returns a 64-byte Uint8Array for a valid keypair array", () => {
    process.env[ENV] = bytes(64);
    const out = parseOperatorKeypairBytes();
    expect(out).toBeInstanceOf(Uint8Array);
    expect(out.length).toBe(64);
  });

  it("trims surrounding whitespace before parsing", () => {
    process.env[ENV] = `  ${bytes(64)}\n`;
    expect(parseOperatorKeypairBytes().length).toBe(64);
  });

  it("throws 'not set' when the env var is missing", () => {
    expect(() => parseOperatorKeypairBytes()).toThrow(/is not set/);
  });

  it("throws 'not set' when the env var is empty or whitespace-only", () => {
    process.env[ENV] = "   ";
    expect(() => parseOperatorKeypairBytes()).toThrow(/is not set/);
  });

  it("rejects a 64-char string that is not a JSON array", () => {
    process.env[ENV] = JSON.stringify("x".repeat(64));
    expect(() => parseOperatorKeypairBytes()).toThrow(/64-byte JSON array/);
  });

  it("rejects a JSON object", () => {
    process.env[ENV] = JSON.stringify({ k: 1 });
    expect(() => parseOperatorKeypairBytes()).toThrow(/64-byte JSON array/);
  });

  it("rejects an array of the wrong length", () => {
    process.env[ENV] = bytes(63);
    expect(() => parseOperatorKeypairBytes()).toThrow(/64-byte JSON array/);
    process.env[ENV] = bytes(65);
    expect(() => parseOperatorKeypairBytes()).toThrow(/64-byte JSON array/);
  });
});
