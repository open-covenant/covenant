import { PublicKey } from "@solana/web3.js";
import { describe, expect, it } from "vitest";
import {
  buylockVaultAuthorityPda,
  configPda,
  deriveAta,
  feeRouterPda,
  lockedVaultAuthorityPda,
  positionPda,
  rewardVaultPda,
} from "../pdas";
import { SPL_TOKEN_PROGRAM_ID } from "../env";

const owner = new PublicKey(new Uint8Array(32).fill(7));
const mint = new PublicKey(new Uint8Array(32).fill(11));

describe("static PDAs", () => {
  it("derives the canonical config and vault addresses", () => {
    expect(configPda().toBase58()).toBe("CNrBUGqrdj5WDTqfBPwyzURBmVThTfWTejqxSqme8EyC");
    expect(feeRouterPda().toBase58()).toBe("x5dMA3DariqtYRc9XMkGhPTMWiXQRjAFZTS9QZLif33");
    expect(rewardVaultPda().toBase58()).toBe("Bh3YKatgy4Sug1g24uFMHrgTQJ8tPFNqKQUqp4sPd4pn");
    expect(lockedVaultAuthorityPda().toBase58()).toBe("BfKVwAzAGpQnwWe5vTkHDnYUKsnjWR4F7CpZcUbgeFrz");
    expect(buylockVaultAuthorityPda().toBase58()).toBe("D9ceemTHkfha5QPgnLjxnaTCtdNLvuPo3fP5g6kZTJK5");
  });
});

describe("positionPda", () => {
  it("derives a position address from owner and a little-endian u64 nonce", () => {
    expect(positionPda(owner, 5n).toBase58()).toBe("ADwco2bCv6i8BK596Ka8BPRZNWJR425CBfDKX77cb4LZ");
  });

  it("derives a distinct address for a different nonce", () => {
    expect(positionPda(owner, 6n).toBase58()).not.toBe(positionPda(owner, 5n).toBase58());
  });
});

describe("deriveAta", () => {
  it("derives the canonical associated-token address for an explicit token program", () => {
    expect(deriveAta(owner, mint, SPL_TOKEN_PROGRAM_ID).toBase58()).toBe(
      "97QynJuxpjCtqa6GZxAXU5YzjXxnXmqxs4aCsjzbceXt",
    );
  });
});
