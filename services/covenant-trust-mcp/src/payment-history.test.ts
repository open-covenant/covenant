import assert from "node:assert/strict";
import test from "node:test";

import {
  PAYAI_FEE_PAYER,
  computePaymentHistory,
  getPaymentHistory,
  parseTransfers,
  type PaymentHistoryCoverage,
  type SponsoredTransfer,
} from "./payment-history.js";

const USDC_MAINNET = "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v";
const USDC_DEVNET = "4zMMC9srt5Ri5X14GAgXhaHii3GnPAEERYPJgZJDncDU";
const OBSERVED_AT = "2026-07-31T08:00:00.000Z";

function transfer(
  payer: string,
  payTo: string,
  amountMicro: bigint,
  signature = "sig",
  slot = 1,
): SponsoredTransfer {
  return {
    signature,
    slot,
    blockTime: null,
    mint: USDC_MAINNET,
    payer,
    payTo,
    amountMicro,
  };
}

function usdcTransaction(
  feePayer: string,
  payer: string,
  payTo: string,
  amount: string,
  slot: number,
): unknown {
  return {
    slot,
    blockTime: 1_785_000_000,
    meta: {
      err: null,
      postTokenBalances: [
        { accountIndex: 1, mint: USDC_MAINNET, owner: payer },
        { accountIndex: 2, mint: USDC_MAINNET, owner: payTo },
      ],
    },
    transaction: {
      message: {
        accountKeys: [
          { pubkey: feePayer },
          { pubkey: `${payer}Ata` },
          { pubkey: `${payTo}Ata` },
          { pubkey: PAYAI_FEE_PAYER },
        ],
        instructions: [
          {
            program: "spl-token",
            parsed: {
              type: "transferChecked",
              info: {
                source: `${payer}Ata`,
                destination: `${payTo}Ata`,
                mint: USDC_MAINNET,
                authority: payer,
                tokenAmount: { amount, decimals: 6 },
              },
            },
          },
        ],
      },
    },
  };
}

test("computePaymentHistory reports inbound transfers with explicit coverage", () => {
  const scanCoverage: PaymentHistoryCoverage = {
    signatures_requested: 100,
    signatures_returned: 5,
    signatures_candidates: 4,
    signatures_scanned: 4,
    signatures_unavailable: 0,
    oldest_slot: 10,
    newest_slot: 20,
  };
  const result = computePaymentHistory(
    [
      transfer("buyer-a", "seller", 2_000_000n, "sig-a", 10),
      transfer("buyer-b", "seller", 3_000_000n, "sig-b", 20),
      transfer("buyer-a", "seller", 1_000_000n, "sig-c", 15),
      transfer("seller", "seller", 9_000_000n, "sig-self", 18),
      transfer("seller", "other", 5_000_000n, "sig-outbound", 19),
    ],
    "seller",
    PAYAI_FEE_PAYER,
    scanCoverage,
    OBSERVED_AT,
  );

  assert.deepEqual(result, {
    wallet: "seller",
    observed_at: OBSERVED_AT,
    observed_inbound_transfers: 3,
    distinct_senders: 2,
    volume_micro_usdc: "6000000",
    observations: [
      {
        transaction_signature: "sig-a",
        slot: 10,
        block_time: null,
        sender: "buyer-a",
        amount_micro_usdc: "2000000",
        mint: USDC_MAINNET,
      },
      {
        transaction_signature: "sig-b",
        slot: 20,
        block_time: null,
        sender: "buyer-b",
        amount_micro_usdc: "3000000",
        mint: USDC_MAINNET,
      },
      {
        transaction_signature: "sig-c",
        slot: 15,
        block_time: null,
        sender: "buyer-a",
        amount_micro_usdc: "1000000",
        mint: USDC_MAINNET,
      },
    ],
    source_fee_payer: PAYAI_FEE_PAYER,
    classification: "payai-sponsored-usdc-transfer",
    settlement_receipt_linked: false,
    coverage: scanCoverage,
  });
  assert.equal("settled_jobs" in result, false);
});

test("parseTransfers resolves token accounts to wallet owners", () => {
  const tx = usdcTransaction(
    PAYAI_FEE_PAYER,
    "BuyerWallet",
    "SellerWallet",
    "500000",
    321,
  ) as any;
  tx.transaction.message.instructions[0].parsed.info.authority = "Delegate";

  assert.deepEqual(parseTransfers(tx, "sig-1"), [
    {
      signature: "sig-1",
      slot: 321,
      blockTime: 1_785_000_000,
      mint: USDC_MAINNET,
      payer: "BuyerWallet",
      payTo: "SellerWallet",
      amountMicro: 500_000n,
    },
  ]);
});

test("parseTransfers accepts only mainnet USDC with both token-account owners", () => {
  const devnet = usdcTransaction(
    PAYAI_FEE_PAYER,
    "BuyerWallet",
    "SellerWallet",
    "500000",
    321,
  ) as any;
  for (const balance of devnet.meta.postTokenBalances) balance.mint = USDC_DEVNET;
  devnet.transaction.message.instructions[0].parsed.info.mint = USDC_DEVNET;
  assert.deepEqual(parseTransfers(devnet, "sig-devnet"), []);

  const missingOwner = usdcTransaction(
    PAYAI_FEE_PAYER,
    "BuyerWallet",
    "SellerWallet",
    "500000",
    321,
  ) as any;
  delete missingOwner.meta.postTokenBalances[0].owner;
  assert.deepEqual(parseTransfers(missingOwner, "sig-missing-owner"), []);
});

test("getPaymentHistory bounds concurrent RPC work and reports the scanned window", async (t) => {
  const originalFetch = globalThis.fetch;
  t.after(() => {
    globalThis.fetch = originalFetch;
  });

  const signatureEntries = [
    { signature: "sig-touch", slot: 30, err: null },
    { signature: "sig-b", slot: 20, err: null },
    { signature: "sig-a", slot: 10, err: null },
    { signature: "sig-null", slot: 5, err: null },
    ...Array.from({ length: 8 }, (_, index) => ({
      signature: `sig-unavailable-${index}`,
      slot: 100 + index,
      err: null,
    })),
    { signature: "sig-failed", slot: 40, err: { InstructionError: [0, "failed"] } },
    { signature: 42, slot: 2, err: null },
    { signature: "sig-a", slot: 10, err: null },
  ];
  const transactions = new Map<string, unknown>([
    [
      "sig-touch",
      usdcTransaction("OtherFeePayer", "TouchBuyer", "SellerWallet", "9000000", 30),
    ],
    [
      "sig-b",
      usdcTransaction(PAYAI_FEE_PAYER, "BuyerB", "SellerWallet", "2000000", 20),
    ],
    [
      "sig-a",
      usdcTransaction(PAYAI_FEE_PAYER, "BuyerA", "SellerWallet", "3000000", 10),
    ],
    ["sig-null", null],
  ]);
  const transactionRequests: string[] = [];
  let activeTransactions = 0;
  let maxActiveTransactions = 0;
  let requestedLimit: unknown;

  globalThis.fetch = async (_input, init) => {
    const request = JSON.parse(String(init?.body)) as {
      method: string;
      params: [string, { limit?: number }];
    };
    if (request.method === "getSignaturesForAddress") {
      requestedLimit = request.params[1].limit;
      return Response.json({ jsonrpc: "2.0", id: 1, result: signatureEntries });
    }

    assert.equal(request.method, "getTransaction");
    const signature = request.params[0];
    transactionRequests.push(signature);
    activeTransactions += 1;
    maxActiveTransactions = Math.max(maxActiveTransactions, activeTransactions);
    await new Promise((resolve) => setTimeout(resolve, 5));
    activeTransactions -= 1;
    return Response.json({
      jsonrpc: "2.0",
      id: 1,
      result: transactions.get(signature),
    });
  };

  const result = await getPaymentHistory("https://rpc.invalid", 1_000, "SellerWallet", 5_000);
  assert.match(result.observed_at, /^\d{4}-\d{2}-\d{2}T/);

  assert.equal(requestedLimit, 1_000);
  assert.deepEqual(
    new Set(transactionRequests),
    new Set([
      "sig-touch",
      "sig-b",
      "sig-a",
      "sig-null",
      ...Array.from({ length: 8 }, (_, index) => `sig-unavailable-${index}`),
    ]),
  );
  assert.equal(transactionRequests.length, 12);
  assert.equal(maxActiveTransactions, 8);
  const {observed_at: _observedAt, ...stableResult} = result;
  assert.deepEqual(stableResult, {
    wallet: "SellerWallet",
    observed_inbound_transfers: 2,
    distinct_senders: 2,
    volume_micro_usdc: "5000000",
    observations: [
      {
        transaction_signature: "sig-b",
        slot: 20,
        block_time: 1_785_000_000,
        sender: "BuyerB",
        amount_micro_usdc: "2000000",
        mint: USDC_MAINNET,
      },
      {
        transaction_signature: "sig-a",
        slot: 10,
        block_time: 1_785_000_000,
        sender: "BuyerA",
        amount_micro_usdc: "3000000",
        mint: USDC_MAINNET,
      },
    ],
    source_fee_payer: PAYAI_FEE_PAYER,
    classification: "payai-sponsored-usdc-transfer",
    settlement_receipt_linked: false,
    coverage: {
      signatures_requested: 1_000,
      signatures_returned: 15,
      signatures_candidates: 12,
      signatures_scanned: 3,
      signatures_unavailable: 9,
      oldest_slot: 10,
      newest_slot: 30,
    },
  });
});

test("computePaymentHistory preserves volumes above JavaScript's safe integer range", () => {
  const amount = BigInt(Number.MAX_SAFE_INTEGER) + 10n;
  const result = computePaymentHistory(
    [transfer("buyer", "seller", amount)],
    "seller",
    PAYAI_FEE_PAYER,
  );

  assert.equal(result.volume_micro_usdc, amount.toString());
});

test("getPaymentHistory reports individual RPC failures as unavailable coverage", async (t) => {
  const originalFetch = globalThis.fetch;
  t.after(() => {
    globalThis.fetch = originalFetch;
  });

  globalThis.fetch = async (_input, init) => {
    const request = JSON.parse(String(init?.body)) as {method: string; params: [string]};
    if (request.method === "getSignaturesForAddress") {
      return Response.json({
        jsonrpc: "2.0",
        id: 1,
        result: [
          {signature: "available", slot: 10, err: null},
          {signature: "rate-limited", slot: 11, err: null},
        ],
      });
    }
    if (request.params[0] === "rate-limited") {
      return new Response("rate limited", {status: 429});
    }
    return Response.json({
      jsonrpc: "2.0",
      id: 1,
      result: usdcTransaction(PAYAI_FEE_PAYER, "Buyer", "Seller", "1", 10),
    });
  };

  const result = await getPaymentHistory("https://rpc.invalid", 1_000, "Seller", 2);
  assert.equal(result.observed_inbound_transfers, 1);
  assert.equal(result.coverage.signatures_scanned, 1);
  assert.equal(result.coverage.signatures_unavailable, 1);
});

test("getPaymentHistory coalesces concurrent scans of the same window", async (t) => {
  const originalFetch = globalThis.fetch;
  t.after(() => {
    globalThis.fetch = originalFetch;
  });
  let signatureRequests = 0;

  globalThis.fetch = async (_input, init) => {
    const request = JSON.parse(String(init?.body)) as {method: string};
    if (request.method === "getSignaturesForAddress") {
      signatureRequests += 1;
      await new Promise((resolve) => setTimeout(resolve, 5));
      return Response.json({jsonrpc: "2.0", id: 1, result: []});
    }
    throw new Error(`unexpected method ${request.method}`);
  };

  await Promise.all([
    getPaymentHistory("https://coalescing.invalid", 1_000, "WalletA", 3),
    getPaymentHistory("https://coalescing.invalid", 1_000, "WalletB", 3),
  ]);

  assert.equal(signatureRequests, 1);
});
