// Golden vectors mirrored from the covenant-payai Rust crate tests
// (index.rs + reputation.rs + types.rs). If the TS port ever diverges from the
// crate's parsing or scoring, these fail. Run: pnpm test.

import { test } from "node:test";
import assert from "node:assert/strict";
import { parseSettlements, computeReputation, type Settlement } from "./reputation.js";

const USDC = "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v";
const TOKEN_PROGRAM = "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA";
const FEE_PAYER = "2wKupLR9q6wXYppw8Gr2NvWxKBUqm4PPJKkQfoxHDBg4";

function usdcTx(): any {
  return {
    slot: 321,
    blockTime: 1_781_980_000,
    meta: {
      err: null,
      postTokenBalances: [
        { accountIndex: 1, mint: USDC, owner: "BuyerWallet" },
        { accountIndex: 3, mint: USDC, owner: "SellerWallet" },
      ],
    },
    transaction: {
      message: {
        accountKeys: [
          { pubkey: "FeePayer" },
          { pubkey: "BuyerAta" },
          { pubkey: USDC },
          { pubkey: "SellerAta" },
          { pubkey: "BuyerWallet" },
        ],
        instructions: [
          {
            program: "spl-token",
            programId: TOKEN_PROGRAM,
            parsed: {
              type: "transferChecked",
              info: {
                source: "BuyerAta",
                destination: "SellerAta",
                mint: USDC,
                authority: "BuyerWallet",
                tokenAmount: { amount: "500000", decimals: 6 },
              },
            },
          },
        ],
      },
    },
  };
}

const settle = (payer: string, payTo: string, micro: bigint): Settlement => ({
  signature: "sig",
  slot: 1,
  blockTime: 1,
  mint: USDC,
  payer,
  payTo,
  amountMicro: micro,
});

test("parses USDC transfer to owner wallets", () => {
  const s = parseSettlements(usdcTx(), "Sig1");
  assert.equal(s.length, 1);
  assert.equal(s[0].payer, "BuyerWallet");
  assert.equal(s[0].payTo, "SellerWallet");
  assert.equal(s[0].amountMicro, 500_000n);
  assert.equal(s[0].mint, USDC);
  assert.equal(s[0].slot, 321);
  assert.equal(s[0].blockTime, 1_781_980_000);
});

test("failed tx yields nothing", () => {
  const tx = usdcTx();
  tx.meta.err = { InstructionError: [0, "Custom"] };
  assert.equal(parseSettlements(tx, "Sig").length, 0);
});

test("non-USDC transfer ignored", () => {
  const tx = usdcTx();
  tx.transaction.message.instructions[0].parsed.info.mint = "So11111111111111111111111111111111111111112";
  assert.equal(parseSettlements(tx, "Sig").length, 0);
});

test("falls back to ATA when owner unresolved", () => {
  const tx = usdcTx();
  tx.meta.postTokenBalances = [];
  const s = parseSettlements(tx, "Sig");
  assert.equal(s.length, 1);
  assert.equal(s[0].payTo, "SellerAta");
  assert.equal(s[0].payer, "BuyerWallet");
});

test("aggregates inbound only with distinct counterparties", () => {
  const s = [
    settle("buyerA", "seller", 2_000_000n),
    settle("buyerB", "seller", 3_000_000n),
    settle("buyerA", "seller", 1_000_000n),
    settle("buyerC", "other", 9_000_000n),
    settle("seller", "buyerA", 5_000_000n),
  ];
  const rep = computeReputation(s, "seller", FEE_PAYER);
  assert.equal(rep.settled_jobs, 3);
  assert.equal(rep.distinct_counterparties, 2);
  assert.equal(rep.volume_micro_usdc, 6_000_000);
  assert.equal(rep.tier, "bronze");
  assert.equal(rep.source_fee_payer, FEE_PAYER);
});

test("self-payments are excluded", () => {
  const s = [settle("buyerA", "seller", 2_000_000n), settle("seller", "seller", 9_000_000n)];
  const rep = computeReputation(s, "seller", FEE_PAYER);
  assert.equal(rep.settled_jobs, 1);
  assert.equal(rep.distinct_counterparties, 1);
  assert.equal(rep.volume_micro_usdc, 2_000_000);
});

test("unknown wallet is zeroed", () => {
  const rep = computeReputation([settle("a", "b", 1n)], "ghost", FEE_PAYER);
  assert.equal(rep.settled_jobs, 0);
  assert.equal(rep.distinct_counterparties, 0);
  assert.equal(rep.volume_micro_usdc, 0);
  assert.equal(rep.score, 0);
});

test("tier thresholds", () => {
  const u = 1_000_000n;
  assert.equal(computeReputation([settle("a", "s", 999n * u)], "s", FEE_PAYER).tier, "bronze");
  assert.equal(computeReputation([settle("a", "s", 1_000n * u)], "s", FEE_PAYER).tier, "silver");
  assert.equal(computeReputation([settle("a", "s", 10_000n * u)], "s", FEE_PAYER).tier, "gold");
  assert.equal(computeReputation([settle("a", "s", 100_000n * u)], "s", FEE_PAYER).tier, "platinum");
});

test("score is bounded and monotonic", () => {
  const big: Settlement[] = [];
  for (let i = 0; i < 200; i++) big.push(settle(`buyer${i}`, "seller", 1_000_000_000n));
  const rep = computeReputation(big, "seller", FEE_PAYER);
  assert.equal(rep.score, 1000);
  assert.equal(rep.tier, "platinum");
});
