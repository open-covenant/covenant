import { createHash, createPublicKey, verify } from 'node:crypto';
import bs58 from 'bs58';

export const sha256 = value => createHash('sha256').update(value).digest('hex');
export const stable = value => Array.isArray(value) ? '[' + value.map(stable).join(',') + ']'
  : value && typeof value === 'object' ? '{' + Object.keys(value).sort().map(k => JSON.stringify(k) + ':' + stable(value[k])).join(',') + '}' : JSON.stringify(value);
export function assert(condition, message) { if (!condition) throw new Error(message); }

// Reject duplicate keys instead of allowing a parser/signature interpretation split.
export function strictJson(source) {
  let p = 0;
  const space = () => { while (/[\t\r\n ]/.test(source[p] ?? '') && p < source.length) p++; };
  const string = () => {
    const start = p++;
    while (p < source.length) {
      if (source[p] === '\\') { p += 2; continue; }
      if (source[p++] === '"') return JSON.parse(source.slice(start, p));
    }
    throw new Error('unterminated_json_string');
  };
  function value(depth = 0) {
    assert(depth < 128, 'json_too_deep'); space();
    if (source[p] === '"') return string();
    if (source[p] === '{') {
      p++; space(); const result = Object.create(null);
      if (source[p] === '}') { p++; return result; }
      for (;;) {
        space(); assert(source[p] === '"', 'invalid_json'); const key = string();
        assert(!Object.hasOwn(result, key), 'duplicate_json_key'); space(); assert(source[p++] === ':', 'invalid_json');
        result[key] = value(depth + 1); space(); const end = source[p++];
        if (end === '}') return result; assert(end === ',', 'invalid_json');
      }
    }
    if (source[p] === '[') {
      p++; space(); const result = [];
      if (source[p] === ']') { p++; return result; }
      for (;;) { result.push(value(depth + 1)); space(); const end = source[p++]; if (end === ']') return result; assert(end === ',', 'invalid_json'); }
    }
    const match = /^(?:true|false|null|-?(?:0|[1-9][0-9]*)(?:\.[0-9]+)?(?:[eE][+-]?[0-9]+)?)/.exec(source.slice(p));
    assert(match, 'invalid_json'); p += match[0].length; const result = JSON.parse(match[0]);
    assert(typeof result !== 'number' || Number.isFinite(result), 'invalid_json_number'); return result;
  }
  const result = value(); space(); assert(p === source.length, 'invalid_json'); return result;
}

function signed(wrapper, field, domain, actor) {
  assert(wrapper && typeof wrapper[field + '_json'] === 'string', 'missing_signed_bytes');
  const raw = wrapper[field + '_json']; const payload = strictJson(raw);
  assert(stable(payload) === stable(wrapper[field]), 'signed_payload_mismatch');
  assert(payload[actor]?.pubkey === wrapper.signer_pubkey_b58, 'signed_identity_mismatch');
  const key = bs58.decode(wrapper.signer_pubkey_b58); const signature = bs58.decode(wrapper.signature_b58);
  assert(key.length === 32 && signature.length === 64, 'invalid_signature_encoding');
  const publicKey = createPublicKey({ key: Buffer.concat([Buffer.from('302a300506032b6570032100', 'hex'), key]), format: 'der', type: 'spki' });
  assert(verify(null, Buffer.from(domain + raw), publicKey, signature), 'invalid_signature'); return payload;
}

function verifyTariff(job, request, blocks) {
  const tariffs = blocks.filter(b => b.type === 'json' && b.value && Object.hasOwn(b.value, 'inference_tariff'));
  const requirement = job.capability_requirement.inference_tariff;
  if (request.pricing === undefined) {
    assert(tariffs.length === 0 && requirement == null && blocks.length === 2, 'unexpected_inference_tariff');
    return;
  }
  assert(request.pricing === 'model_tariff_v1', 'invalid_pricing');
  assert(tariffs.length === 1 && blocks.length === 3, 'inference_tariff_required');
  assert(Object.keys(tariffs[0].value).length === 1, 'invalid_inference_tariff_block');
  const quote = tariffs[0].value.inference_tariff;
  const fields = (value, names) => value && typeof value === 'object' && !Array.isArray(value)
    && Object.keys(value).sort().join(',') === [...names].sort().join(',');
  assert(fields(quote, ['tariff', 'requested_output_cap', 'price_micro_usdc']), 'invalid_inference_tariff_quote');
  assert(stable(quote) === stable(requirement), 'inference_tariff_requirement_mismatch');
  const tariff = quote.tariff;
  assert(fields(tariff, ['schema_version', 'id', 'revision', 'model', 'base_micro_usdc', 'output_cap_micro_usdc_per_million', 'max_output_tokens', 'valid_from_ms', 'valid_until_ms']), 'invalid_inference_tariff');
  const integer = (value, minimum = 0) => Number.isSafeInteger(value) && value >= minimum;
  assert(tariff.schema_version === 1 && integer(tariff.revision, 1)
    && typeof tariff.id === 'string' && /^[A-Za-z0-9_.-]{1,128}$/.test(tariff.id)
    && typeof tariff.model === 'string' && tariff.model.trim() && tariff.model !== 'any'
    && Buffer.byteLength(tariff.model) <= 128 && !/[\u0000-\u001f\u007f-\u009f]/u.test(tariff.model)
    && integer(tariff.base_micro_usdc) && integer(tariff.output_cap_micro_usdc_per_million)
    && integer(tariff.max_output_tokens, 1) && tariff.max_output_tokens <= 0xffff_ffff
    && integer(tariff.valid_from_ms) && integer(tariff.valid_until_ms) && tariff.valid_until_ms > tariff.valid_from_ms,
  'invalid_inference_tariff');
  const canonicalModel = model => model.endsWith(':latest') ? model.slice(0, -7) : model;
  assert(canonicalModel(tariff.model) === canonicalModel(request.model), 'inference_tariff_model_mismatch');
  assert(integer(job.issued_at_ms) && tariff.valid_from_ms <= job.issued_at_ms && job.issued_at_ms < tariff.valid_until_ms, 'inference_tariff_issue_time_mismatch');
  assert(integer(quote.requested_output_cap, 1) && quote.requested_output_cap === request.max_tokens && quote.requested_output_cap <= tariff.max_output_tokens, 'inference_tariff_cap_mismatch');
  const priceFor = cap => BigInt(tariff.base_micro_usdc) + (BigInt(cap) * BigInt(tariff.output_cap_micro_usdc_per_million) + 999_999n) / 1_000_000n;
  assert(priceFor(tariff.max_output_tokens) <= BigInt(Number.MAX_SAFE_INTEGER)
    && integer(quote.price_micro_usdc, 1) && priceFor(quote.requested_output_cap) === BigInt(quote.price_micro_usdc)
    && quote.price_micro_usdc === job.price_micro_usdc, 'inference_tariff_price_mismatch');
}

export function verifyComputeResult(result, { request, requestSha256, quoteId, gatewayBuyer, operators }) {
  return verifyNativeResult(result, { request, gatewayBuyer, operators, requestSha256, quoteId });
}

// Batch children are authenticated by the ordered aggregate proof and signed
// native envelope rather than single-purchase gateway metadata.
export function verifyNativeResult(result, { request, requestSha256, quoteId, gatewayBuyer, operators, batchChild = false }) {
  const proof = result.covenant;
  assert(proof && (batchChild || (proof.request_sha256 === requestSha256 && proof.quote_id === quoteId)), 'result_request_binding_mismatch');
  const job = signed(proof.envelope, 'payload', 'covenant.compute.job.v1\n', 'buyer');
  const receipt = signed(proof.receipt, 'receipt', 'covenant.compute.receipt.v1\n', 'operator');
  assert(job.buyer.pubkey === gatewayBuyer, 'unexpected_gateway_buyer');
  assert(operators.includes(receipt.operator.pubkey), 'unexpected_operator');
  assert(job.kind === 'inference_call' && job.capability_requirement?.model_id === request.model, 'job_model_mismatch');
  assert(receipt.job_id === job.job_id && proof.job_id === job.job_id, 'receipt_job_id_mismatch');
  assert(receipt.job_hash_hex === sha256(proof.envelope.payload_json), 'receipt_job_hash_mismatch');
  assert(typeof proof.output_json === 'string' && receipt.result_hash_hex === sha256(proof.output_json), 'receipt_output_hash_mismatch');
  const output = strictJson(proof.output_json);
  assert(stable(output) === stable(proof.output), 'output_bytes_mismatch');
  const blocks = job.input;
  assert(Array.isArray(blocks), 'invalid_job_input');
  verifyTariff(job, request, blocks);
  const chats = blocks.filter(b => b.type === 'json' && b.value?.messages);
  const generations = blocks.filter(b => b.type === 'json' && b.value?.generation);
  assert(chats.length === 1 && stable(chats[0].value.messages) === stable(request.messages), 'job_messages_mismatch');
  assert(generations.length === 1, 'job_generation_missing');
  assert(chats[0] !== generations[0], 'invalid_job_input');
  const generation = generations[0].value.generation;
  assert(Object.keys(generation).every(key => ['max_tokens', 'temperature'].includes(key)), 'unexpected_generation_parameters');
  assert(generation.max_tokens === request.max_tokens, 'job_token_limit_mismatch');
  assert((generation.temperature ?? null) === (request.temperature ?? null), 'job_temperature_mismatch');
  assert(Number.isSafeInteger(job.price_micro_usdc) && job.price_micro_usdc > 0 && Number.isSafeInteger(receipt.price_micro_usdc) && receipt.price_micro_usdc >= 0 && receipt.price_micro_usdc <= job.price_micro_usdc, 'receipt_price_mismatch');
  assert(receipt.status === 'ok', 'compute_job_not_completed');
  const text = output.filter(b => b.type === 'text').map(b => b.text).join('\n');
  assert(text && result.choices?.[0]?.message?.content === text, 'completion_output_mismatch');
  return { receipt, job, text };
}
