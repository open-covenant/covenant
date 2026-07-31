// Transfer parsing and bounded summary fixtures. These deliberately avoid
// reputation or job semantics: the input only proves observed token transfers.

import { test } from 'node:test';
import assert from 'node:assert/strict';
import {
  parseUsdcTransfers,
  summarizeTransferActivity,
  type TransferObservation,
} from './reputation.js';

const USDC = 'EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v';
const TOKEN_PROGRAM = 'TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA';
const FEE_PAYER = '2wKupLR9q6wXYppw8Gr2NvWxKBUqm4PPJKkQfoxHDBg4';

function usdcTx(): any {
  return {
    slot: 321,
    blockTime: 1_781_980_000,
    meta: {
      err: null,
      postTokenBalances: [
        { accountIndex: 1, mint: USDC, owner: 'BuyerWallet' },
        { accountIndex: 3, mint: USDC, owner: 'SellerWallet' },
      ],
    },
    transaction: {
      message: {
        accountKeys: [
          { pubkey: 'FeePayer' },
          { pubkey: 'BuyerAta' },
          { pubkey: USDC },
          { pubkey: 'SellerAta' },
          { pubkey: 'BuyerWallet' },
        ],
        instructions: [
          {
            program: 'spl-token',
            programId: TOKEN_PROGRAM,
            parsed: {
              type: 'transferChecked',
              info: {
                source: 'BuyerAta',
                destination: 'SellerAta',
                mint: USDC,
                authority: 'BuyerWallet',
                tokenAmount: { amount: '500000', decimals: 6 },
              },
            },
          },
        ],
      },
    },
  };
}

const transfer = (payer: string, payTo: string, micro: bigint): TransferObservation => ({
  signature: 'sig',
  slot: 1,
  blockTime: 1,
  mint: USDC,
  payer,
  payTo,
  amountMicro: micro,
});

test('parses USDC transfer to owner wallets', () => {
  const s = parseUsdcTransfers(usdcTx(), 'Sig1');
  assert.equal(s.length, 1);
  assert.equal(s[0].payer, 'BuyerWallet');
  assert.equal(s[0].payTo, 'SellerWallet');
  assert.equal(s[0].amountMicro, 500_000n);
  assert.equal(s[0].mint, USDC);
  assert.equal(s[0].slot, 321);
  assert.equal(s[0].blockTime, 1_781_980_000);
});

test('failed tx yields nothing', () => {
  const tx = usdcTx();
  tx.meta.err = { InstructionError: [0, 'Custom'] };
  assert.equal(parseUsdcTransfers(tx, 'Sig').length, 0);
});

test('non-USDC transfer ignored', () => {
  const tx = usdcTx();
  tx.transaction.message.instructions[0].parsed.info.mint =
    'So11111111111111111111111111111111111111112';
  assert.equal(parseUsdcTransfers(tx, 'Sig').length, 0);
});

test('falls back to ATA when owner unresolved', () => {
  const tx = usdcTx();
  tx.meta.postTokenBalances = [];
  const s = parseUsdcTransfers(tx, 'Sig');
  assert.equal(s.length, 1);
  assert.equal(s[0].payTo, 'SellerAta');
  assert.equal(s[0].payer, 'BuyerWallet');
});

test('aggregates inbound only with distinct counterparties', () => {
  const s = [
    transfer('buyerA', 'seller', 2_000_000n),
    transfer('buyerB', 'seller', 3_000_000n),
    transfer('buyerA', 'seller', 1_000_000n),
    transfer('buyerC', 'other', 9_000_000n),
    transfer('seller', 'buyerA', 5_000_000n),
  ];
  const activity = summarizeTransferActivity(s, 'seller', FEE_PAYER, {
    requested: 100,
    returned: 42,
    loaded: 40,
  });
  assert.equal(activity.observed_inbound_transfers, 3);
  assert.equal(activity.distinct_observed_senders, 2);
  assert.equal(activity.observed_volume_micro_usdc, '6000000');
  assert.equal(activity.source_account_scanned, FEE_PAYER);
  assert.deepEqual(activity.coverage, {
    requested_signature_limit: 100,
    signatures_returned: 42,
    transactions_loaded: 40,
    commitment: 'confirmed',
  });
});

test('self-payments are excluded', () => {
  const s = [transfer('buyerA', 'seller', 2_000_000n), transfer('seller', 'seller', 9_000_000n)];
  const activity = summarizeTransferActivity(s, 'seller', FEE_PAYER);
  assert.equal(activity.observed_inbound_transfers, 1);
  assert.equal(activity.distinct_observed_senders, 1);
  assert.equal(activity.observed_volume_micro_usdc, '2000000');
});

test('unknown wallet is zeroed', () => {
  const activity = summarizeTransferActivity([transfer('a', 'b', 1n)], 'ghost', FEE_PAYER);
  assert.equal(activity.observed_inbound_transfers, 0);
  assert.equal(activity.distinct_observed_senders, 0);
  assert.equal(activity.observed_volume_micro_usdc, '0');
});

test('keeps large atomic-unit totals exact', () => {
  const value = BigInt(Number.MAX_SAFE_INTEGER) + 42n;
  const activity = summarizeTransferActivity([transfer('a', 's', value)], 's', FEE_PAYER);
  assert.equal(activity.observed_volume_micro_usdc, value.toString());
});
