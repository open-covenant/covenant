import { PublicKey } from "@solana/web3.js";
import { describe, expect, it } from "vitest";
import {
  TIER_7D_BPS,
  TIER_30D_BPS,
  TIER_90D_BPS,
  TIER_180D_BPS,
  TIER_OPTIONS,
  buildClaimIx,
  buildClosePositionIx,
  buildCreateAtaIx,
  buildCreatePositionIx,
} from "../txBuilder";
import { configPda, positionPda } from "../pdas";

const STAKE_PROGRAM = "CstkpU2q9RngbHh21WVAYeQjbN9UWgcH9pAiQcMaEcED";
const ATA_PROGRAM = "ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL";

const owner = new PublicKey(new Uint8Array(32).fill(7));
const ownerCvntAccount = new PublicKey(new Uint8Array(32).fill(8));
const payer = new PublicKey(new Uint8Array(32).fill(20));

const flags = (ix: { keys: { isSigner: boolean; isWritable: boolean }[] }) =>
  ix.keys.map((k) => [k.isSigner, k.isWritable]);

const loneSigner = (ix: { keys: { pubkey: PublicKey; isSigner: boolean }[] }) => {
  const signers = ix.keys.filter((k) => k.isSigner);
  expect(signers).toHaveLength(1);
  return signers[0].pubkey;
};

describe("tier constants", () => {
  it("exposes the four lock-tier bps values", () => {
    expect([TIER_7D_BPS, TIER_30D_BPS, TIER_90D_BPS, TIER_180D_BPS]).toEqual([5_000, 10_000, 15_000, 20_000]);
  });

  it("lists the four tier options with matching bps", () => {
    expect(TIER_OPTIONS.map((t) => t.bps)).toEqual([5_000, 10_000, 15_000, 20_000]);
    expect(TIER_OPTIONS.map((t) => t.days)).toEqual([7, 30, 90, 180]);
  });
});

describe("buildCreatePositionIx", () => {
  const ix = buildCreatePositionIx({
    owner,
    ownerCvntAccount,
    nonce: 5n,
    amount: 1_000_000n,
    lockTierBps: 10_000,
  });

  it("encodes the discriminator then nonce, amount, and tier little-endian", () => {
    expect(Array.from(ix.data)).toEqual([
      48, 215, 197, 153, 96, 203, 180, 133,
      5, 0, 0, 0, 0, 0, 0, 0,
      64, 66, 15, 0, 0, 0, 0, 0,
      16, 39,
    ]);
  });

  it("targets the stake program with the expected account-meta layout", () => {
    expect(ix.programId.toBase58()).toBe(STAKE_PROGRAM);
    expect(ix.keys).toHaveLength(9);
    expect(flags(ix)).toEqual([
      [false, true],
      [false, true],
      [false, false],
      [false, false],
      [false, true],
      [false, true],
      [true, true],
      [false, false],
      [false, false],
    ]);
  });

  it("wires config and position into their slots and makes only the owner sign", () => {
    expect(ix.keys[0].pubkey.equals(configPda())).toBe(true);
    expect(ix.keys[1].pubkey.equals(positionPda(owner, 5n))).toBe(true);
    expect(loneSigner(ix).equals(owner)).toBe(true);
  });
});

describe("buildClaimIx", () => {
  const ix = buildClaimIx({ owner, nonce: 5n });

  it("encodes the bare claim discriminator", () => {
    expect(Array.from(ix.data)).toEqual([62, 198, 214, 193, 213, 159, 108, 210]);
  });

  it("targets the stake program with four accounts, owner-signed", () => {
    expect(ix.programId.toBase58()).toBe(STAKE_PROGRAM);
    expect(ix.keys).toHaveLength(4);
    expect(flags(ix)).toEqual([
      [false, true],
      [false, true],
      [false, true],
      [true, true],
    ]);
    expect(loneSigner(ix).equals(owner)).toBe(true);
  });
});

describe("buildClosePositionIx", () => {
  const ix = buildClosePositionIx({ owner, nonce: 5n });

  it("encodes the bare close_position discriminator", () => {
    expect(Array.from(ix.data)).toEqual([123, 134, 81, 0, 49, 68, 98, 98]);
  });

  it("targets the stake program with nine accounts, owner-signed", () => {
    expect(ix.programId.toBase58()).toBe(STAKE_PROGRAM);
    expect(ix.keys).toHaveLength(9);
    expect(flags(ix)).toEqual([
      [false, true],
      [false, true],
      [false, false],
      [false, false],
      [false, true],
      [false, true],
      [false, true],
      [true, true],
      [false, false],
    ]);
    expect(loneSigner(ix).equals(owner)).toBe(true);
  });
});

describe("buildCreateAtaIx", () => {
  const ix = buildCreateAtaIx({ payer, owner });

  it("carries no instruction data and targets the associated-token program", () => {
    expect(Array.from(ix.data)).toEqual([]);
    expect(ix.programId.toBase58()).toBe(ATA_PROGRAM);
  });

  it("lays out six accounts with only the payer signing", () => {
    expect(ix.keys).toHaveLength(6);
    expect(flags(ix)).toEqual([
      [true, true],
      [false, true],
      [false, false],
      [false, false],
      [false, false],
      [false, false],
    ]);
    expect(loneSigner(ix).equals(payer)).toBe(true);
  });
});
