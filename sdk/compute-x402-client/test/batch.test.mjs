import test from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { randomUUID } from 'node:crypto';
import { CovenantX402Client, shortMemo, LEGACY_PAYMENT_COMPUTE_UNITS, PAYMENT_COMPUTE_UNITS } from '../index.mjs';
import { verifyBatchProof, verifyBatchResult } from '../batch.mjs';
import { sha256, stable, strictJson } from '../proof.mjs';
import { batchProof, fixture, input } from './fixtures/mock-gateway.mjs';

const child = (model = 'small-model', max_tokens = 128, pricing = 'model_tariff_v1') => {
  const { requestId, ...request } = input();
  return { ...request, model, max_tokens, ...(pricing ? { pricing } : {}) };
};
const batch = (requests = [child(), child('large-model', 256)]) => ({ requestId: randomUUID(), version: 1, requests });
const config = f => ({ ...f.config, maxPaymentMicroUsdc: 40_000, maxTotalMicroUsdc: 80_000 });

// Fixture produced independently by the Rust protocol test suite. It establishes
// proof interoperability, separately from the signed-native and payment tests.
test('Rust batch proof vector verifies in the released client', () => {
  const vector = strictJson(readFileSync(new URL('../../../fixtures/compute/inference-batch-v1.json', import.meta.url), 'utf8'));
  verifyBatchProof(vector.result);
});

test('RFC6962 includes every ordered leaf for all supported tree sizes', () => {
  for (let size = 1; size <= 32; size++) {
    const outputs = Array.from({ length: size }, (_, index) => ({ covenant: { job_id: randomUUID() }, text: `答案 ${index}` }));
    const result = batchProof(randomUUID(), 'ab'.repeat(32), outputs);
    verifyBatchProof(result);
    for (const item of result.items) {
      const original = item.output.text; item.output.text = 'changed';
      assert.throws(() => verifyBatchProof(result), /batch_output_hash_mismatch/); item.output.text = original;
    }
  }
});

test('one signed payment purchases ordered mixed tariff and legacy results with native receipts', async () => {
  const f = await fixture({ childPrice: request => 1000 + request.max_tokens });
  const client = new CovenantX402Client(config(f)), request = batch([child(), child('legacy-model', 512, null)]);
  try {
    const result = await client.purchaseBatch(request);
    assert.equal(result.status, 'completed'); assert.equal(result.texts.length, 2);
    assert.equal(result.amountMicroUsdc, 2640);
    assert.deepEqual(result.result.items.map(item => item.output.covenant.envelope.payload.capability_requirement.model_id), request.requests.map(item => item.model));
    assert.equal(f.counts().quotes, 1); assert.equal(f.counts().signatures, 1); assert.equal(f.counts().sends, 1);
    assert.equal(result.result.tree_size, 2);
    const before = f.counts().requests;
    assert.equal(JSON.stringify(await client.purchaseBatch(request)), JSON.stringify(result));
    assert.equal(f.counts().requests, before);
  } finally { client.close(); await f.cleanup(); }
});

for (const [label, mutate, error] of [
  ['missing child', result => { result.items.pop(); }, 'invalid_or_incomplete_batch'],
  ['changed order', result => { result.items.reverse(); }, 'batch_item_binding_mismatch'],
  ['changed output', result => { result.items[0].output.choices[0].message.content = 'changed'; }, 'batch_output_hash_mismatch'],
  ['changed root', result => { result.merkle_root = 'ff'.repeat(32); }, 'invalid_batch_proof'],
  ['extra proof node', result => { result.items[0].audit_path.push('aa'.repeat(32)); }, 'invalid_batch_proof'],
  ['wrong batch ID', result => { result.batch_id = randomUUID(); }, 'batch_identity_mismatch'],
  ['wrong request digest', result => { result.request_sha256 = 'bb'.repeat(32); }, 'batch_request_mismatch'],
]) {
  test(`batch refuses ${label} and retains the original payment and budget`, async () => {
    const f = await fixture({ badResult: mutate }); const client = new CovenantX402Client(config(f)); const request = batch();
    try {
      await assert.rejects(client.purchaseBatch(request), new RegExp(error));
      const saved = client.journal.get(request.requestId);
      assert.ok(saved.payment); assert.equal(saved.amount, 2000); assert.equal(saved.result, null);
      await assert.rejects(client.purchaseBatch(request), new RegExp(error));
      assert.equal(f.counts().quotes, 1); assert.equal(f.counts().signatures, 1);
      assert.equal(f.counts().payments[0], f.counts().payments[1]);
    } finally { client.close(); await f.cleanup(); }
  });
}

for (const [label, mutate, error] of [
  ['native signature', value => { value.covenant.receipt.signature_b58 = '1'.repeat(64); }, 'invalid_signature'],
  ['displayed output', value => { value.choices[0].message.content = 'changed'; }, 'completion_output_mismatch'],
]) {
  test(`a self-consistent Merkle tree cannot replace child ${label}`, async () => {
    const f = await fixture({ badChild: (value, index) => { if (index === 1) mutate(value); } });
    const client = new CovenantX402Client(config(f)); const request = batch();
    try { await assert.rejects(client.purchaseBatch(request), new RegExp(error)); assert.equal(client.journal.get(request.requestId).result, null); }
    finally { client.close(); await f.cleanup(); }
  });
}

test('signed native tariff downgrade is refused even with a consistent batch proof', async () => {
  const f = await fixture({ badJob: (job, index) => { if (index === 1) { job.input.pop(); delete job.capability_requirement.inference_tariff; } } });
  const client = new CovenantX402Client(config(f)); const request = batch();
  try { await assert.rejects(client.purchaseBatch(request), /inference_tariff_required/); assert.ok(client.journal.get(request.requestId).payment); }
  finally { client.close(); await f.cleanup(); }
});

for (const pendingFirst of [true, false]) {
  test(`${pendingFirst ? 'pending' : 'failed transport'} batch resumes the frozen quote and authorization after restart and repricing`, async () => {
    let price = 1000; const f = await fixture({ pendingFirst, dropFirst: !pendingFirst, childPrice: () => price });
    let client = new CovenantX402Client(config(f)); const request = batch();
    try {
      if (pendingFirst) assert.equal((await client.purchaseBatch(request)).status, 'pending');
      else await assert.rejects(client.purchaseBatch(request), /authorization_retained/);
      const original = client.journal.get(request.requestId); price = 5000;
      client.close(); client = new CovenantX402Client(config(f));
      for (const changed of [batch([...request.requests].reverse()), batch([child('new-model'), request.requests[1]]), batch([child('small-model', 129), request.requests[1]])]) {
        await assert.rejects(client.purchaseBatch({ ...changed, requestId: request.requestId }), /idempotency_request_mismatch/);
      }
      const result = await client.purchaseBatch(request); const saved = client.journal.get(request.requestId);
      assert.equal(result.amountMicroUsdc, 2000); assert.equal(saved.quote, original.quote); assert.equal(saved.payment, original.payment);
      assert.equal(f.counts().quotes, 1); assert.equal(f.counts().signatures, 1);
      assert.equal((await client.purchaseBatch(batch())).amountMicroUsdc, 10_000);
    } finally { client.close(); await f.cleanup(); }
  });
}

test('single and batch purchases share the original cumulative journal budget and request IDs', async () => {
  const f = await fixture(); const client = new CovenantX402Client({ ...config(f), maxPaymentMicroUsdc: 3000, maxTotalMicroUsdc: 3000 });
  const request = batch();
  try {
    await client.purchaseBatch(request); await client.purchase(input());
    await assert.rejects(client.purchase(input()), /cumulative_spend_cap_exceeded/);
    await assert.rejects(client.purchase({ ...input(), requestId: request.requestId }), /idempotency_request_mismatch/);
    assert.equal(f.counts().signatures, 2);
  } finally { client.close(); await f.cleanup(); }
});

for (const [label, mutate, error] of [
  ['missing manifest', quote => { delete quote.extra.batchPricing; }, 'invalid_batch_quote'],
  ['changed retail sum', quote => { quote.extra.batchPricing.children[0].amount_atomic++; }, 'batch_quote_amount_mismatch'],
  ['wrong model', quote => { quote.extra.batchPricing.children[0].model = 'other'; }, 'batch_quote_request_mismatch'],
  ['wrong cap', quote => { quote.extra.batchPricing.children[0].max_output_tokens++; }, 'batch_quote_request_mismatch'],
  ['duplicate child identity', quote => { quote.extra.batchPricing.children[1].job_id = quote.extra.batchPricing.children[0].job_id; }, 'invalid_batch_quote_child'],
]) {
  test(`batch quote rejects ${label} before signing`, async () => {
    const f = await fixture({ badQuote: mutate }); const client = new CovenantX402Client(config(f));
    try { await assert.rejects(client.purchaseBatch(batch()), new RegExp(error)); assert.equal(f.counts().signatures, 0); assert.equal(f.counts().sends, 0); }
    finally { client.close(); await f.cleanup(); }
  });
}

test('completed replay rejects altered texts without network calls', async () => {
  const f = await fixture(); const client = new CovenantX402Client(config(f)); const request = batch();
  try {
    const result = await client.purchaseBatch(request); result.texts.reverse(); result.texts[0] = 'replacement';
    client.journal.db.prepare('UPDATE purchases SET result=? WHERE id=?').run(JSON.stringify(result), request.requestId);
    const before = f.counts().requests;
    await assert.rejects(client.purchaseBatch(request), /cached_text_mismatch/);
    assert.equal(f.counts().requests, before);
  } finally { client.close(); await f.cleanup(); }
});

for (const requests of [[], Array.from({ length: 33 }, () => child()), [{ version: 1, requests: [child()] }]]) {
  test(`invalid batch shape ${requests.length} refuses before journal or RPC`, async () => {
    const f = await fixture(); const client = new CovenantX402Client(config(f)); const request = batch(requests);
    try { await assert.rejects(client.purchaseBatch(request)); assert.equal(f.counts().requests, 0); assert.equal(client.journal.get(request.requestId), undefined); }
    finally { client.close(); await f.cleanup(); }
  });
}


test('shared signed batch fixture validates native tariffs and the frozen retail manifest', () => {
  const f = strictJson(readFileSync(new URL('./fixtures/signed-batch-response.json', import.meta.url), 'utf8'));
  const result = verifyBatchResult(f.response, { request: f.request, requestSha256: sha256(stable(f.request)), quote: f.quote, gatewayBuyer: f.gatewayBuyer, operators: f.operators });
  assert.equal(result.texts.length, 2);
  const wrongQuote = structuredClone(f.quote); wrongQuote.extra.requestSha256 = 'cc'.repeat(32);
  assert.throws(() => verifyBatchResult(f.response, { request: f.request, quote: wrongQuote, gatewayBuyer: f.gatewayBuyer, operators: f.operators }), /batch_request_mismatch/);
  assert.notEqual(result.jobs.reduce((sum, job) => sum + job.price_micro_usdc, 0), Number(f.quote.amount));
  const oldResult = structuredClone(f.response);
  const newQuote = structuredClone(f.quote); newQuote.extra.batchPricing.children[0].job_id = randomUUID();
  assert.throws(() => verifyBatchResult(oldResult, { request: f.request, requestSha256: sha256(stable(f.request)), quote: newQuote, gatewayBuyer: f.gatewayBuyer, operators: f.operators }), /batch_child_identity_mismatch/);
});

test('a signed failed child cannot complete or release the aggregate reservation', async () => {
  const f = await fixture({ badReceipt: (receipt, index) => { if (index === 1) receipt.status = 'failed'; } });
  const client = new CovenantX402Client(config(f)); const request = batch();
  try {
    await assert.rejects(client.purchaseBatch(request), /compute_job_not_completed/);
    const row = client.journal.get(request.requestId); assert.ok(row.payment); assert.equal(row.result, null); assert.equal(row.amount, 2000);
  } finally { client.close(); await f.cleanup(); }
});

test('batch cannot accept payment instructions for the single completion endpoint', async () => {
  const f = await fixture({ badRequired: required => { required.resource.url = required.resource.url.replace('/v1/inference/batches', '/v1/chat/completions'); } });
  const client = new CovenantX402Client(config(f));
  try { await assert.rejects(client.purchaseBatch(batch()), /untrusted_payment_resource/); assert.equal(f.counts().signatures, 0); }
  finally { client.close(); await f.cleanup(); }
});

test('a historical authorized 32-child batch reuses its one original payment', async () => {
  const f = await fixture(); const client = new CovenantX402Client(config(f));
  try {
    const request = batch(Array.from({ length: 32 }, (_, index) => child('model-' + index)));
    await seedHistoricalAuthorization(client, request);
    const result = await client.purchaseBatch(request);
    assert.equal(result.texts.length, 32); assert.equal(result.amountMicroUsdc, 32000); assert.equal(f.counts().signatures, 1);
  } finally { client.close(); await f.cleanup(); }
});

test('batch input limits refuse before RPC and preserve existing authorization', async () => {
  const f = await fixture(); const client = new CovenantX402Client(config(f));
  try {
    const oversized = child(); oversized.messages = [{ role: 'user', content: '💰'.repeat(1100) }];
    const request = batch([oversized]);
    await assert.rejects(client.purchaseBatch(request), /input_byte_limit_exceeded/);
    assert.equal(client.journal.get(request.requestId), undefined);
    const underChildLimit = child(); underChildLimit.messages = [{ role: 'user', content: 'x'.repeat(4000) }];
    const aggregate = batch(Array.from({ length: 32 }, () => underChildLimit));
    // A historical payment is necessary to reach the aggregate limit now that
    // fresh admissions are capped at two. A placeholder is never sent.
    const body = stable({ version: aggregate.version, requests: aggregate.requests });
    const row = client.journal.claim(aggregate.requestId, body, sha256(body));
    client.journal.db.prepare('UPDATE purchases SET payment=? WHERE id=?').run('historical-placeholder', aggregate.requestId);
    client.journal.release(aggregate.requestId, row.owner);
    await assert.rejects(client.purchaseBatch(aggregate), /request_too_large/);
    assert.equal(client.journal.get(aggregate.requestId).quote, null); assert.equal(f.counts().requests, 0);
  } finally { client.close(); await f.cleanup(); }
});


test('bounded batch responses may exceed the single-response limit', async () => {
  const f = await fixture({ outputText: 'x'.repeat(50_000) }); const client = new CovenantX402Client(config(f));
  try {
    const request = batch(Array.from({ length: 32 }, () => child()));
    await seedHistoricalAuthorization(client, request);
    const result = await client.purchaseBatch(request);
    assert.ok(Buffer.byteLength(JSON.stringify(result.result)) > 4 * 1024 * 1024);
    assert.equal(result.texts.length, 32); assert.equal(result.texts[0].length, 50_000);
  } finally { client.close(); await f.cleanup(); }
});

test('batch responses beyond the hard limit retain the original authorization and reservation', async () => {
  const f = await fixture({ outputText: 'x'.repeat(120_000) }); const client = new CovenantX402Client(config(f));
  const request = batch(Array.from({ length: 32 }, () => child()));
  try {
    await seedHistoricalAuthorization(client, request);
    await assert.rejects(client.purchaseBatch(request), /response_too_large/);
    const row = client.journal.get(request.requestId); assert.ok(row.payment); assert.equal(row.amount, 32000); assert.equal(row.result, null);
    assert.equal(f.counts().signatures, 1);
  } finally { client.close(); await f.cleanup(); }
});


async function seedHistoricalAuthorization(client, { requestId, ...request }) {
  const body = stable(request), digest = sha256(body);
  const row = client.journal.claim(requestId, body, digest);
  try {
    const response = await client.post(requestId, body, undefined, client.batchEndpoint);
    const required = strictJson(Buffer.from(response.headers.get('PAYMENT-REQUIRED'), 'base64').toString());
    await response.body.cancel();
    const { price } = client.validateQuote(required, digest, false, client.batchEndpoint);
    client.journal.reserve(requestId, row.owner, required, price);
    const payload = await client.http.createPaymentPayload(required);
    const payment = new Headers(client.http.encodePaymentSignatureHeader(payload)).get('PAYMENT-SIGNATURE');
    client.journal.signed(requestId, row.owner, payment);
  } finally { client.journal.release(requestId, row.owner); }
}

test('three fresh requests are refused before quote, signature or budget reservation', async () => {
  const f = await fixture(); const client = new CovenantX402Client(config(f)); const request = batch([child(), child(), child()]);
  try {
    await assert.rejects(client.purchaseBatch(request), /fresh_batch_size_exceeded/);
    assert.equal(client.journal.get(request.requestId), undefined); assert.equal(f.counts().requests, 0); assert.equal(f.counts().signatures, 0);
  } finally { client.close(); await f.cleanup(); }
});


test('single purchases reject batch pricing before signing or reserving money', async () => {
  const f = await fixture({ badQuote: quote => { quote.extra.batchPricing = {}; } });
  const client = new CovenantX402Client(config(f)); const request = input();
  try {
    await assert.rejects(client.purchase(request), /unexpected_batch_pricing/);
    assert.equal(f.counts().signatures, 0); assert.equal(f.counts().sends, 0); assert.equal(client.journal.get(request.requestId).amount, 0);
  } finally { client.close(); await f.cleanup(); }
});


for (const legacyMemo of [false, true]) {
  test(`${legacyMemo ? 'legacy long' : 'standard short'} batch memo survives mixed-network quote selection and pending recovery`, async () => {
    const f = await fixture({ pendingFirst: true, legacyMemo,
      expectedUnits: legacyMemo ? LEGACY_PAYMENT_COMPUTE_UNITS : PAYMENT_COMPUTE_UNITS,
      offers: quote => [{ scheme: 'exact', network: 'eip155:8453', extra: { quoteId: 'unrelated-other-network' } }, quote] });
    let client = new CovenantX402Client(config(f)); const request = batch();
    try {
      assert.equal((await client.purchaseBatch(request)).status, 'pending');
      const original = client.journal.get(request.requestId);
      const selected = JSON.parse(original.quote).accepts[1];
      assert.equal(selected.extra.memo, legacyMemo ? `covenant.x402.v2:${selected.extra.quoteId}:${original.digest}` : shortMemo(selected.extra.quoteId, original.digest));
      client.close(); client = new CovenantX402Client(config(f));
      const completed = await client.purchaseBatch(request);
      assert.equal(completed.status, 'completed');
      const recovered = client.journal.get(request.requestId);
      assert.equal(recovered.quote, original.quote); assert.equal(recovered.payment, original.payment);
      assert.equal(f.counts().signatures, 1); assert.equal(f.counts().quotes, 1);
      assert.equal(f.counts().payments[0], f.counts().payments[1]);
      const before = f.counts().requests;
      assert.equal(stable(await client.purchaseBatch(request)), stable(completed));
      assert.equal(f.counts().requests, before);
    } finally { client.close(); await f.cleanup(); }
  });
}

for (const [label, options, error] of [
  ['a short memo for another request', { badQuote: quote => { quote.extra.memo = shortMemo(quote.extra.quoteId, 'ff'.repeat(32)); } }, 'quote_memo_mismatch'],
  ['two Solana offers', { offers: quote => [quote, structuredClone(quote)] }, 'ambiguous_payment_requirements'],
  ['no Solana offer', { offers: () => [{ network: 'eip155:8453' }] }, 'untrusted_payment_rail'],
]) test(`batch refuses ${label} before wallet or budget reservation`, async () => {
  const f = await fixture(options), client = new CovenantX402Client(config(f)), request = batch();
  try {
    await assert.rejects(client.purchaseBatch(request), new RegExp(error));
    assert.equal(client.journal.get(request.requestId).quote, null);
    assert.equal(f.counts().signatures, 0); assert.equal(f.counts().sends, 0);
  } finally { client.close(); await f.cleanup(); }
});

test('standalone batch verification binds both memo formats to the original quote and ordered request', () => {
  const f = strictJson(readFileSync(new URL('./fixtures/signed-batch-response.json', import.meta.url), 'utf8'));
  const digest = sha256(stable(f.request));
  const verify = quote => verifyBatchResult(f.response, { request: f.request, quote, gatewayBuyer: f.gatewayBuyer, operators: f.operators });
  for (const memo of [shortMemo(f.quote.extra.quoteId, digest), `covenant.x402.v2:${f.quote.extra.quoteId}:${digest}`]) {
    const quote = structuredClone(f.quote); quote.extra.memo = memo;
    assert.equal(verify(quote).texts.length, 2);
    quote.extra.memo = memo + '0'; assert.throws(() => verify(quote), /quote_memo_mismatch/);
  }
  const changed = structuredClone(f.quote); changed.extra.quoteId = randomUUID();
  assert.throws(() => verify(changed), /quote_memo_mismatch/);
  changed.extra.quoteId = '00000000-0000-0000-0000-000000000000';
  assert.throws(() => verify(changed), /batch_request_mismatch/);
});
