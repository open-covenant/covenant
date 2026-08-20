import { PublicKey } from "@solana/web3.js";
import { describe, expect, it } from "vitest";
import {
  computePendingLamports,
  decodeConfig,
  decodeStakePosition,
  ownerPositionFilter,
  pickStakeSource,
  sumBalances,
  type OwnedTokenAccount,
  type StakePositionState,
} from "../readers";

const pk = (fill: number) => new PublicKey(new Uint8Array(32).fill(fill));

class Cursor {
  readonly buf: Uint8Array;
  private readonly view: DataView;
  off = 0;
  constructor(size: number) {
    this.buf = new Uint8Array(size);
    this.view = new DataView(this.buf.buffer);
  }
  skip(n: number) {
    this.off += n;
    return this;
  }
  bool(v: boolean) {
    this.buf[this.off] = v ? 1 : 0;
    this.off += 1;
    return this;
  }
  pubkey(p: PublicKey) {
    this.buf.set(p.toBytes(), this.off);
    this.off += 32;
    return this;
  }
  u16(v: number) {
    this.view.setUint16(this.off, v, true);
    this.off += 2;
    return this;
  }
  u32(v: number) {
    this.view.setUint32(this.off, v, true);
    this.off += 4;
    return this;
  }
  u64(v: bigint) {
    this.view.setBigUint64(this.off, v, true);
    this.off += 8;
    return this;
  }
  i64(v: bigint) {
    this.view.setBigInt64(this.off, v, true);
    this.off += 8;
    return this;
  }
  u128(v: bigint) {
    this.view.setBigUint64(this.off, v & 0xffffffffffffffffn, true);
    this.view.setBigUint64(this.off + 8, v >> 64n, true);
    this.off += 16;
    return this;
  }
}

describe("decodeConfig", () => {
  it("reads every field at its fixed offset", () => {
    const buf = new Cursor(193)
      .skip(8)
      .pubkey(pk(1))
      .pubkey(pk(2))
      .bool(true)
      .pubkey(pk(3))
      .u64(1000n)
      .u32(50)
      .u32(7)
      .u128(123_456_789n)
      .u128(999n)
      .u64(4242n)
      .i64(1_700_000_000n)
      .u64(5_000_000_000n)
      .u64(88n)
      .i64(1_600_000_000n).buf;

    const cfg = decodeConfig(buf);
    expect(cfg.authority.equals(pk(1))).toBe(true);
    expect(cfg.pauseAuthority.equals(pk(2))).toBe(true);
    expect(cfg.paused).toBe(true);
    expect(cfg.covntMint.equals(pk(3))).toBe(true);
    expect(cfg.minLockAmount).toBe(1000n);
    expect(cfg.maxActiveLocks).toBe(50);
    expect(cfg.activeLockCount).toBe(7);
    expect(cfg.accSolPerWeight).toBe(123_456_789n);
    expect(cfg.totalWeight).toBe(999n);
    expect(cfg.pendingSolLamports).toBe(4242n);
    expect(cfg.lastAccrualTs).toBe(1_700_000_000n);
    expect(cfg.cumulativeSolDistributed).toBe(5_000_000_000n);
    expect(cfg.cumulativeBuylockCvnt).toBe(88n);
    expect(cfg.initializedTs).toBe(1_600_000_000n);
  });
});

describe("decodeStakePosition", () => {
  it("carries the supplied pubkey and reads every field at its fixed offset", () => {
    const buf = new Cursor(123)
      .skip(8)
      .pubkey(pk(9))
      .u64(3n)
      .u64(1_000_000n)
      .u128(2_000_000_000_000n)
      .u16(15_000)
      .i64(1_650_000_000n)
      .i64(1_660_000_000n)
      .u128(500n)
      .u64(250n)
      .i64(1_640_000_000n).buf;

    const pos = decodeStakePosition(pk(8), buf);
    expect(pos.pubkey.equals(pk(8))).toBe(true);
    expect(pos.owner.equals(pk(9))).toBe(true);
    expect(pos.nonce).toBe(3n);
    expect(pos.amount).toBe(1_000_000n);
    expect(pos.weight).toBe(2_000_000_000_000n);
    expect(pos.multiplierBps).toBe(15_000);
    expect(pos.lockStart).toBe(1_650_000_000n);
    expect(pos.lockEnd).toBe(1_660_000_000n);
    expect(pos.rewardDebt).toBe(500n);
    expect(pos.unclaimedLamports).toBe(250n);
    expect(pos.createdAt).toBe(1_640_000_000n);
  });
});

describe("computePendingLamports", () => {
  const base: StakePositionState = {
    pubkey: pk(0),
    owner: pk(0),
    nonce: 0n,
    amount: 0n,
    weight: 0n,
    multiplierBps: 0,
    lockStart: 0n,
    lockEnd: 0n,
    rewardDebt: 0n,
    unclaimedLamports: 0n,
    createdAt: 0n,
  };
  const SCALE = 1_000_000_000_000n;

  it("accrues gross minus debt plus the unclaimed carry", () => {
    expect(computePendingLamports({ ...base, weight: SCALE, rewardDebt: 3n, unclaimedLamports: 2n }, 10n)).toBe(9n);
  });

  it("returns just the unclaimed carry at the debt boundary", () => {
    expect(computePendingLamports({ ...base, weight: SCALE, rewardDebt: 5n, unclaimedLamports: 2n }, 5n)).toBe(2n);
  });

  it("guards against debt underflow by returning the unclaimed carry", () => {
    expect(computePendingLamports({ ...base, weight: SCALE, rewardDebt: 5n, unclaimedLamports: 7n }, 1n)).toBe(7n);
  });

  it("scales weight by accSolPerWeight over ACC_SCALE", () => {
    expect(computePendingLamports({ ...base, weight: 2n * SCALE }, 5n)).toBe(10n);
  });
});

describe("pickStakeSource", () => {
  const acc = (amount: bigint, fill: number): OwnedTokenAccount => ({ pubkey: pk(fill), amount });

  it("returns null for an empty set", () => {
    expect(pickStakeSource([])).toBeNull();
  });

  it("picks the largest balance", () => {
    const best = pickStakeSource([acc(1n, 1), acc(5n, 2), acc(3n, 3)]);
    expect(best?.amount).toBe(5n);
    expect(best?.pubkey.equals(pk(2))).toBe(true);
  });

  it("keeps the first account on a tie", () => {
    const best = pickStakeSource([acc(5n, 1), acc(5n, 2)]);
    expect(best?.pubkey.equals(pk(1))).toBe(true);
  });
});

describe("sumBalances", () => {
  it("returns 0n for an empty set", () => {
    expect(sumBalances([])).toBe(0n);
  });

  it("folds all balances", () => {
    expect(sumBalances([
      { pubkey: pk(1), amount: 1n },
      { pubkey: pk(2), amount: 2n },
      { pubkey: pk(3), amount: 3n },
    ])).toBe(6n);
  });
});

describe("ownerPositionFilter", () => {
  it("builds a memcmp at offset 8 keyed by the base58 owner", () => {
    const owner = pk(4);
    const filter = ownerPositionFilter(owner);
    expect(filter.memcmp.offset).toBe(8);
    expect(filter.memcmp.bytes).toBe(owner.toBase58());
  });
});
