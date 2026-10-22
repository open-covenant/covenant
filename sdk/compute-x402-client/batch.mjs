import { createHash } from 'node:crypto';
import { assert, sha256, stable, verifyNativeResult } from './proof.mjs';

const fields = (value, names) => value && typeof value === 'object' && !Array.isArray(value)
  && Object.keys(value).sort().join(',') === [...names].sort().join(',');
const uuid = value => typeof value === 'string' && /^[a-f0-9]{8}(-[a-f0-9]{4}){3}-[a-f0-9]{12}$/.test(value)
  && value !== '00000000-0000-0000-0000-000000000000';
const bytes = value => {
  assert(typeof value === 'string' && /^[a-f0-9]{64}$/.test(value), 'invalid_batch_digest');
  return Buffer.from(value, 'hex');
};
const hash = value => createHash('sha256').update(value).digest();

// Retail prices and native operator tariffs are separate. The one accepted
// payment freezes each retail amount and child identity, not a native-price sum.
export function verifyBatchQuote(quote, request) {
  const digest = sha256(stable(request));
  const extra = quote.extra;
  assert(extra && uuid(extra.quoteId) && extra.requestSha256 === digest, 'batch_request_mismatch');
  // Standalone receipt verification has the same request-bound memo policy as
  // the purchase path. Saved long memos remain valid and are never rewritten.
  const shortMemo = sha256(`covenant.x402.v3:${extra.quoteId}:${digest}`).slice(0, 32);
  assert(extra.memo === shortMemo || extra.memo === `covenant.x402.v2:${extra.quoteId}:${digest}`, 'quote_memo_mismatch');
  const pricing = extra.batchPricing;
  assert(fields(pricing, ['version', 'batch_id', 'children']) && pricing.version === 1 && uuid(pricing.batch_id)
    && Array.isArray(pricing.children) && pricing.children.length === request.requests.length, 'invalid_batch_quote');
  const ids = new Set(); let total = 0n;
  for (const [index, child] of pricing.children.entries()) {
    const expected = request.requests[index];
    assert(child && typeof child === 'object' && !Array.isArray(child), 'invalid_batch_quote_child');
    const names = ['job_id', 'model', 'max_output_tokens', 'amount_atomic'];
    if (Object.hasOwn(child, 'pricing')) names.push('pricing');
    assert(fields(child, names) && uuid(child.job_id) && !ids.has(child.job_id), 'invalid_batch_quote_child');
    ids.add(child.job_id);
    assert(child.model === expected.model && child.max_output_tokens === expected.max_tokens
      && (child.pricing ?? null) === (expected.pricing ?? null), 'batch_quote_request_mismatch');
    assert(Number.isSafeInteger(child.amount_atomic) && child.amount_atomic > 0, 'invalid_batch_quote_amount');
    total += BigInt(child.amount_atomic);
  }
  assert(total <= BigInt(Number.MAX_SAFE_INTEGER) && total.toString() === quote.amount, 'batch_quote_amount_mismatch');
  return pricing;
}

// RFC6962 self-consistency alone does not authenticate a result. Native signed
// receipts are checked below, and the paid quote fixes batch and child IDs.
export function verifyBatchProof(result, { requestSha256, batchPricing, treeSize } = {}) {
  assert(fields(result, ['version', 'batch_id', 'request_sha256', 'tree_size', 'merkle_root', 'items'])
    && result.version === 1 && uuid(result.batch_id) && Number.isSafeInteger(result.tree_size)
    && result.tree_size >= 1 && result.tree_size <= 32 && Array.isArray(result.items)
    && result.items.length === result.tree_size, 'invalid_or_incomplete_batch');
  assert(treeSize === undefined || result.tree_size === treeSize, 'batch_size_mismatch');
  bytes(result.request_sha256); bytes(result.merkle_root);
  assert(requestSha256 === undefined || result.request_sha256 === requestSha256, 'batch_request_mismatch');
  assert(batchPricing === undefined || result.batch_id === batchPricing.batch_id, 'batch_identity_mismatch');
  const ids = new Set();
  for (const [index, item] of result.items.entries()) {
    assert(fields(item, ['leaf', 'output', 'audit_path']), 'invalid_batch_item');
    const leaf = item.leaf;
    assert(fields(leaf, ['version', 'batch_id', 'request_sha256', 'index', 'job_id', 'result_sha256'])
      && leaf.version === 1 && leaf.index === index && leaf.batch_id === result.batch_id
      && leaf.request_sha256 === result.request_sha256 && uuid(leaf.job_id) && !ids.has(leaf.job_id), 'batch_item_binding_mismatch');
    ids.add(leaf.job_id);
    assert(batchPricing === undefined || leaf.job_id === batchPricing.children[index]?.job_id, 'batch_child_identity_mismatch');
    assert(leaf.result_sha256 === sha256(stable(item.output)), 'batch_output_hash_mismatch');
    let value = hash(Buffer.concat([Buffer.from([0]), Buffer.from('covenant.compute.inference-batch.leaf.v1\n' + stable(leaf))]));
    let position = index, last = result.tree_size - 1;
    assert(Array.isArray(item.audit_path) && item.audit_path.length <= 5, 'invalid_batch_proof');
    for (const siblingHex of item.audit_path) {
      assert(last !== 0, 'invalid_batch_proof'); const sibling = bytes(siblingHex);
      if ((position & 1) === 1 || position === last) {
        value = hash(Buffer.concat([Buffer.from([1]), sibling, value]));
        if ((position & 1) === 0) while ((position & 1) === 0 && position !== 0) { position >>= 1; last >>= 1; }
      } else value = hash(Buffer.concat([Buffer.from([1]), value, sibling]));
      position >>= 1; last >>= 1;
    }
    assert(last === 0 && value.toString('hex') === result.merkle_root, 'invalid_batch_proof');
  }
  return result;
}

export function verifyBatchResult(result, { request, requestSha256, quote, gatewayBuyer, operators }) {
  const digest = sha256(stable(request));
  assert((requestSha256 === undefined || requestSha256 === digest) && quote?.extra?.requestSha256 === digest, 'batch_request_mismatch');
  const batchPricing = verifyBatchQuote(quote, request);
  verifyBatchProof(result, { requestSha256: digest, batchPricing, treeSize: request.requests.length });
  const verified = result.items.map((item, index) => {
    const native = verifyNativeResult(item.output, { request: request.requests[index], gatewayBuyer, operators, batchChild: true });
    assert(native.job.job_id === item.leaf.job_id && native.job.capability_requirement.kind === 'inference_call'
      && (native.job.stream === undefined || native.job.stream === false), 'batch_native_request_mismatch');
    return native;
  });
  return { texts: verified.map(item => item.text), jobs: verified.map(item => item.job), receipts: verified.map(item => item.receipt) };
}
