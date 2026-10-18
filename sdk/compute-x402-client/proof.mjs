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

export function verifyComputeResult(result, { request, requestSha256, quoteId, gatewayBuyer, operators }) {
  const proof = result.covenant;
  assert(proof?.request_sha256 === requestSha256 && proof.quote_id === quoteId, 'result_request_binding_mismatch');
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
  assert(Array.isArray(blocks) && blocks.length === 2, 'invalid_job_input');
  const chats = blocks.filter(b => b.type === 'json' && b.value?.messages);
  const generations = blocks.filter(b => b.type === 'json' && b.value?.generation);
  assert(chats.length === 1 && stable(chats[0].value.messages) === stable(request.messages), 'job_messages_mismatch');
  assert(generations.length === 1, 'job_generation_missing');
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
