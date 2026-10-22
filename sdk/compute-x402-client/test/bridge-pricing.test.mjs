import test from 'node:test';
import assert from 'node:assert/strict';
import { pathToFileURL } from 'node:url';
import { Keypair, VersionedTransaction } from '@solana/web3.js';
import { createKeyPairSignerFromBytes } from '@solana/kit';
import { toFacilitatorSvmSigner } from '@x402/svm';
import { CovenantX402Client, SOLANA_GENESIS, NETWORK, USDC } from '../index.mjs';
import { sha256 } from '../proof.mjs';
import { fixture, input } from './fixtures/mock-gateway.mjs';

test('SDK tariff and legacy payments pass one independently configured real facilitator', {
  skip: !process.env.COVENANT_X402_BRIDGE_PATH,
}, async () => {
  const { createBridge, Rejected } = await import(pathToFileURL(process.env.COVENANT_X402_BRIDGE_PATH).href);
  const f = await fixture({ price: body => body.pricing ? 12000 + 9 * body.max_tokens : 50000 });
  const client = new CovenantX402Client({ ...f.config, maxPaymentMicroUsdc: 50000, maxTotalMicroUsdc: 100000 });
  try {
    const requests = [
      { ...input(), model: 'claude-haiku-4.5', max_tokens: 256, pricing: 'model_tariff_v1' },
      { ...input(), model: 'claude-haiku-4.5', max_tokens: 2048, pricing: 'model_tariff_v1' },
      { ...input(), model: 'claude-haiku-4.5', max_tokens: 256 },
    ];
    const amounts = [14304, 30432, 50000];
    for (let i = 0; i < requests.length; i++) {
      assert.equal((await client.purchase(requests[i])).amountMicroUsdc, amounts[i]);
    }
    const payments = f.counts().payments.map(wire => JSON.parse(Buffer.from(wire, 'base64')));
    const first = payments[0], now = Date.now();
    const tariff = { schema_version: 1, revision: 1, model: 'claude-haiku-4.5', max_output_tokens: 2048,
      valid_from_ms: now - 60000, valid_until_ms: now + 3600000 };
    // Independent deployment policy: never infer the bridge price from a payment.
    const config = { network: NETWORK, asset: USDC, payTo: f.config.payTo,
      feePayer: first.accepted.extra.feePayer, resourceUrl: f.config.gatewayUrl + '/v1/chat/completions',
      priceAtomic: '50000', secret: 'unfunded-test-secret-with-more-than-32-chars', tariffRows: [{
        native: { ...tariff, id: 'fixture-native', base_micro_usdc: 10000, output_cap_micro_usdc_per_million: 7500000 },
        retail: { ...tariff, id: 'fixture-retail', base_micro_usdc: 12000, output_cap_micro_usdc_per_million: 9000000 },
      }] };
    const signer = toFacilitatorSvmSigner(await createKeyPairSignerFromBytes(Uint8Array.from(f.feeKeypair)));
    let sponsorSignatures = 0, simulations = 0;
    const signTransaction = signer.signTransaction.bind(signer);
    signer.signTransaction = async (...args) => { sponsorSignatures++; return signTransaction(...args); };
    signer.simulateTransaction = async () => { simulations++; };
    const rpcMethods = [];
    const bridge = createBridge({ signer, config, rpc: async method => {
      rpcMethods.push(method);
      assert.equal(method, 'getGenesisHash', 'verification must never broadcast');
      return SOLANA_GENESIS;
    } });
    const verify = (payment, terms = payment.accepted) => bridge.verify({
      paymentPayload: payment, paymentRequirements: terms, resourceUrl: config.resourceUrl,
    });
    for (let i = 0; i < payments.length; i++) {
      assert.equal(payments[i].accepted.amount, String(amounts[i]));
      const verified = await verify(payments[i]);
      assert.equal(verified.payer, f.walletProvider.getAddress());
      assert.equal(verified.authorization_id, sha256(Buffer.from(payments[i].payload.transaction, 'base64')));
    }
    assert.equal(sponsorSignatures, 3);
    assert.equal(simulations, 3);

    const nonmember = structuredClone(first);
    nonmember.accepted.amount = '14305'; // Between valid integer-cap tariff prices.
    const changedAccepted = structuredClone(first);
    changedAccepted.accepted.amount = '30432'; // Valid policy price, wrong saved quote.
    const changedTransfer = structuredClone(first);
    const tx = VersionedTransaction.deserialize(Buffer.from(changedTransfer.payload.transaction, 'base64'));
    const transfer = tx.message.compiledInstructions.find(ix =>
      tx.message.staticAccountKeys[ix.programIdIndex].toBase58() === 'TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA');
    Buffer.from(transfer.data.buffer, transfer.data.byteOffset, transfer.data.byteLength).writeBigUInt64LE(30432n, 1);
    tx.signatures = tx.signatures.map(() => new Uint8Array(64));
    tx.sign([Keypair.fromSecretKey(Uint8Array.from(f.walletKeypair))]);
    changedTransfer.payload.transaction = Buffer.from(tx.serialize()).toString('base64');
    for (const [payment, terms] of [[nonmember, nonmember.accepted], [changedAccepted, first.accepted], [changedTransfer, first.accepted]]) {
      await assert.rejects(verify(payment, terms), Rejected);
      assert.equal(sponsorSignatures, 3, 'rejected payments must not consume sponsor signatures');
      assert.equal(simulations, 3, 'static rejection must precede RPC simulation');
    }
    assert(rpcMethods.every(method => method === 'getGenesisHash'));
    assert.equal(f.counts().signatures, 3, 'exactly one SDK buyer signature per original request');
  } finally {
    client.close();
    await f.cleanup();
  }
});


for (const legacyMemo of [false, true]) test(`official SVM ${legacyMemo ? 'legacy long' : 'standard short'} memo batch payment passes independent facilitator policy before signed batch completion`, {
  skip: !process.env.COVENANT_X402_BRIDGE_PATH,
}, async () => {
  const { createBridge, Rejected } = await import(pathToFileURL(process.env.COVENANT_X402_BRIDGE_PATH).href);
  let bridge, verified, sponsorSignatures = 0, simulations = 0;
  const f = await fixture({
    legacyMemo, expectedUnits: legacyMemo ? 60_000 : 20_000,
    childPrice: request => 12000 + 9 * request.max_tokens,
    verifyPayment: async (paymentPayload, resourceUrl) => {
      verified = await bridge.verify({ paymentPayload, paymentRequirements: paymentPayload.accepted, resourceUrl });
    },
  });
  const client = new CovenantX402Client({ ...f.config, maxPaymentMicroUsdc: 50000, maxTotalMicroUsdc: 50000 });
  try {
    const now = Date.now();
    const tariff = { schema_version: 1, revision: 1, model: 'claude-haiku-4.5', max_output_tokens: 2048,
      valid_from_ms: now - 60000, valid_until_ms: now + 3600000 };
    const config = { network: NETWORK, asset: USDC, payTo: f.config.payTo,
      feePayer: Keypair.fromSecretKey(Uint8Array.from(f.feeKeypair)).publicKey.toBase58(),
      resourceUrl: f.config.gatewayUrl + '/v1/chat/completions', priceAtomic: '50000', batchEnabled: true,
      secret: 'unfunded-test-secret-with-more-than-32-chars', tariffRows: [{
        native: { ...tariff, id: 'fixture-native', base_micro_usdc: 10000, output_cap_micro_usdc_per_million: 7500000 },
        retail: { ...tariff, id: 'fixture-retail', base_micro_usdc: 12000, output_cap_micro_usdc_per_million: 9000000 },
      }] };
    const signer = toFacilitatorSvmSigner(await createKeyPairSignerFromBytes(Uint8Array.from(f.feeKeypair)));
    const signTransaction = signer.signTransaction.bind(signer);
    signer.signTransaction = async (...args) => { sponsorSignatures++; return signTransaction(...args); };
    signer.simulateTransaction = async () => { simulations++; };
    bridge = createBridge({ signer, config, rpc: async method => {
      assert.equal(method, 'getGenesisHash', 'verification must never broadcast'); return SOLANA_GENESIS;
    } });
    const { requestId, ...request } = input();
    const purchase = { requestId, version: 1, requests: [256, 2048].map(max_tokens => ({
      ...request, model: 'claude-haiku-4.5', max_tokens, pricing: 'model_tariff_v1',
    })) };
    const complete = await client.purchaseBatch(purchase);
    assert.equal(complete.status, 'completed'); assert.equal(complete.amountMicroUsdc, 44736);
    assert.equal(complete.texts.length, 2);
    const payment = JSON.parse(Buffer.from(f.counts().payments[0], 'base64'));
    assert.equal(payment.resource.url, f.config.gatewayUrl + '/v1/inference/batches');
    assert.deepEqual(payment.accepted.extra.batchPricing.children.map(child => child.amount_atomic), [14304, 30432]);
    assert.equal(verified.payer, f.walletProvider.getAddress());
    assert.equal(verified.authorization_id, sha256(Buffer.from(payment.payload.transaction, 'base64')));
    assert.equal(sponsorSignatures, 1); assert.equal(simulations, 1); assert.equal(f.counts().signatures, 1);
    const before = f.counts().requests;
    assert.equal((await client.purchaseBatch(purchase)).status, 'completed');
    assert.equal(f.counts().requests, before);
    for (const mutation of [
      value => { value.accepted.extra.batchPricing.children.reverse(); },
      value => { value.resource.url = config.resourceUrl; },
    ]) {
      const changed = structuredClone(payment); mutation(changed);
      await assert.rejects(bridge.verify({ paymentPayload: changed, paymentRequirements: payment.accepted, resourceUrl: payment.resource.url }), Rejected);
      assert.equal(sponsorSignatures, 1); assert.equal(simulations, 1);
    }
  } finally { client.close(); await f.cleanup(); }
});
