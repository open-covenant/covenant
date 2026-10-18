import { x402Client, x402HTTPClient } from '@x402/core/client';
import { ExactSvmScheme } from '@x402/svm/exact/client';
import { address, getTransactionEncoder } from '@solana/kit';
import { VersionedTransaction } from '@solana/web3.js';
import bs58 from 'bs58';
import { z } from 'zod';
import { assert, sha256, stable, strictJson, verifyComputeResult } from './proof.mjs';
import { Journal } from './journal.mjs';

export const SOLANA_GENESIS = '5eykt4UsFv8P8NJdTREpY1vzqKqZKvdpKuc147dw2N9d';
export const NETWORK = 'solana:5eykt4UsFv8P8NJdTREpY1vzqKqZKvdp';
export const USDC = 'EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v';
export const requestSchema = z.object({
  model: z.string().min(1).max(200),
  messages: z.array(z.object({ role: z.enum(['system', 'user', 'assistant']), content: z.string().min(1).max(64_000) }).strict()).min(1).max(32),
  max_tokens: z.number().int().min(1).max(8192),
  temperature: z.number().finite().min(0).max(2).multipleOf(0.01).optional(),
}).strict();
const amount = value => { assert(Number.isSafeInteger(value) && value > 0, 'invalid_spend_cap'); return value; };
function url(value, local = false) {
  const u = new URL(value);
  assert(!u.username && !u.password && !u.hash, 'invalid_url');
  assert(u.protocol === 'https:' || (local && u.protocol === 'http:' && ['localhost', '127.0.0.1', '[::1]'].includes(u.hostname)), 'https_required');
  return u;
}

// The request-bound memo needs more than the upstream client's default 20k CU.
// Kit's modifying-signer contract returns the changed message as well as its
// signature; returning signatures alone would invalidate the SDK's final wire.
export const PAYMENT_COMPUTE_UNITS = 60_000;
export function agentKitSvmSigner(wallet) {
  const network = wallet.getNetwork();
  assert(network.protocolFamily === 'svm' && network.chainId === SOLANA_GENESIS, 'agentkit_wallet_not_solana_mainnet');
  const publicKey = wallet.getAddress(); address(publicKey);
  assert(typeof wallet.signTransaction === 'function', 'agentkit_wallet_cannot_sign_transactions');
  return {
    address: address(publicKey),
    async modifyAndSignTransactions(transactions) {
      const results = [];
      for (const transaction of transactions) {
        const wire = getTransactionEncoder().encode(transaction);
        const unsigned = VersionedTransaction.deserialize(wire);
        assert(unsigned.signatures.every(signature => signature.every(byte => byte === 0)), 'payment_must_be_unsigned_before_compute_limit_change');
        const limits = unsigned.message.compiledInstructions.filter(instruction =>
          unsigned.message.staticAccountKeys[instruction.programIdIndex].toBase58() === 'ComputeBudget111111111111111111111111111111' && instruction.data[0] === 2);
        const limit = limits[0];
        assert(limits.length === 1 && unsigned.message.compiledInstructions[0] === limit && limit.accountKeyIndexes.length === 0 && limit.data.length === 5 && Buffer.from(limit.data).readUInt32LE(1) === 20_000, 'unexpected_payment_compute_limit');
        // No account, transfer, memo, priority fee or lifetime field is changed.
        const data = Buffer.from(limit.data); data.writeUInt32LE(PAYMENT_COMPUTE_UNITS, 1); limit.data = data;
        const message = Buffer.from(unsigned.message.serialize());
        const signed = await wallet.signTransaction(unsigned);
        assert(Buffer.from(signed.message.serialize()).equals(message), 'wallet_changed_payment_transaction');
        const signerIndex = signed.message.staticAccountKeys.findIndex(key => key.toBase58() === publicKey);
        assert(signerIndex >= 0 && signerIndex < signed.message.header.numRequiredSignatures, 'wallet_not_required_signer');
        const signature = signed.signatures[signerIndex];
        assert(signature?.length === 64 && signature.some(byte => byte !== 0), 'wallet_returned_no_signature');
        results.push({ ...transaction, messageBytes: Uint8Array.from(message),
          signatures: { ...transaction.signatures, [publicKey]: Uint8Array.from(signature) } });
      }
      return results;
    },
  };
}

async function bodyText(response, limit = 4 * 1024 * 1024) {
  let size = 0; const chunks = [];
  for await (const chunk of response.body ?? []) { size += chunk.length; assert(size <= limit, 'response_too_large'); chunks.push(chunk); }
  return Buffer.concat(chunks).toString('utf8');
}

export class CovenantX402Client {
  constructor({ gatewayUrl, rpcUrl, payTo, gatewayBuyer, operators, walletProvider, journalPath,
    maxPaymentMicroUsdc, maxTotalMicroUsdc, maxInputBytes = 4096, timeoutMs = 30_000, fetchImpl = fetch, allowLocalhost = false }) {
    const gateway = url(gatewayUrl, allowLocalhost);
    assert(gateway.pathname === '/' && !gateway.search, 'gateway_origin_required');
    this.endpoint = gateway.origin + '/v1/chat/completions'; this.rpcUrl = url(rpcUrl, allowLocalhost).href;
    address(payTo); address(gatewayBuyer);
    assert(Array.isArray(operators) && operators.length > 0, 'trusted_operator_keys_required'); operators.forEach(address);
    this.payTo = payTo; this.gatewayBuyer = gatewayBuyer; this.operators = [...operators]; this.wallet = walletProvider;
    this.maxPayment = amount(maxPaymentMicroUsdc); this.maxTotal = amount(maxTotalMicroUsdc);
    assert(Number.isSafeInteger(maxInputBytes) && maxInputBytes > 0 && maxInputBytes <= 65_536, 'invalid_input_byte_limit'); this.maxInputBytes = maxInputBytes;
    assert(this.maxPayment <= this.maxTotal, 'payment_cap_exceeds_total_cap');
    assert(Number.isSafeInteger(timeoutMs) && timeoutMs > 0 && timeoutMs <= 120_000, 'invalid_timeout');
    this.timeoutMs = timeoutMs; this.fetch = fetchImpl;
    const signer = agentKitSvmSigner(walletProvider); this.payer = signer.address;
    const client = new x402Client().register(NETWORK, new ExactSvmScheme(signer, { rpcUrl: this.rpcUrl }));
    this.http = new x402HTTPClient(client);
    this.journal = new Journal(journalPath, sha256(stable({ payer: this.payer, endpoint: this.endpoint, network: NETWORK, asset: USDC })), this.maxTotal);
  }
  async rpc(method, params = []) {
    const response = await this.fetch(this.rpcUrl, { method: 'POST', redirect: 'error', signal: AbortSignal.timeout(this.timeoutMs),
      headers: { 'content-type': 'application/json' }, body: JSON.stringify({ jsonrpc: '2.0', id: 1, method, params }) });
    assert(response.ok, 'rpc_unavailable'); const parsed = strictJson(await bodyText(response));
    assert(!parsed.error && Object.hasOwn(parsed, 'result'), 'rpc_error'); return parsed.result;
  }
  async post(id, body, payment) {
    return this.fetch(this.endpoint, { method: 'POST', body, redirect: 'error', signal: AbortSignal.timeout(this.timeoutMs),
      headers: { 'content-type': 'application/json', 'idempotency-key': id, ...(payment ? { 'PAYMENT-SIGNATURE': payment } : {}) } });
  }
  validateQuote(required, digest, expiredAllowed = false) {
    assert(required.x402Version === 2 && required.resource?.url === this.endpoint, 'untrusted_payment_resource');
    assert(required.accepts?.length === 1, 'ambiguous_payment_requirements'); const quote = required.accepts[0];
    assert(quote.scheme === 'exact' && quote.network === NETWORK && quote.asset === USDC && quote.payTo === this.payTo, 'untrusted_payment_rail');
    assert(typeof quote.amount === 'string' && /^[1-9][0-9]*$/.test(quote.amount), 'invalid_payment_amount');
    const price = BigInt(quote.amount); assert(price <= BigInt(this.maxPayment), 'per_payment_cap_exceeded');
    const extra = quote.extra;
    assert(extra && /^[0-9a-f-]{36}$/.test(extra.quoteId) && extra.requestSha256 === digest, 'quote_request_mismatch');
    assert(Number.isSafeInteger(extra.expiresAtMs) && (expiredAllowed || extra.expiresAtMs > Date.now() + 5000), 'quote_expired');
    assert(extra.memo === `covenant.x402.v2:${extra.quoteId}:${digest}`, 'quote_memo_mismatch');
    address(extra.feePayer); assert(extra.feePayer !== this.payer, 'buyer_must_not_pay_solana_fees');
    return { quote, price: Number(price) };
  }
  async verifySettlement(header, payment) {
    assert(header?.length < 8192, 'missing_payment_response');
    const settlement = strictJson(Buffer.from(header, 'base64').toString('utf8'));
    assert(settlement.success === true && settlement.network === NETWORK && settlement.payer === this.payer, 'settlement_identity_mismatch');
    assert(typeof settlement.transaction === 'string' && bs58.decode(settlement.transaction).length === 64, 'invalid_settlement_signature');
    const authorization = strictJson(Buffer.from(payment, 'base64').toString('utf8'));
    const signed = VersionedTransaction.deserialize(Buffer.from(authorization.payload.transaction, 'base64'));
    const landed = await this.rpc('getTransaction', [settlement.transaction, { commitment: 'confirmed', encoding: 'base64', maxSupportedTransactionVersion: 0 }]);
    assert(landed?.meta?.err === null && Array.isArray(landed.transaction), 'settlement_not_confirmed');
    const confirmed = VersionedTransaction.deserialize(Buffer.from(landed.transaction[0], 'base64'));
    assert(bs58.encode(confirmed.signatures[0]) === settlement.transaction, 'settlement_signature_mismatch');
    assert(Buffer.from(confirmed.message.serialize()).equals(Buffer.from(signed.message.serialize())), 'settlement_transaction_mismatch');
    return settlement;
  }
  async purchase({ requestId, ...input }) {
    assert(typeof requestId === 'string' && /^[0-9a-f]{8}(-[0-9a-f]{4}){3}-[0-9a-f]{12}$/.test(requestId), 'stable_uuid_request_id_required');
    const request = requestSchema.parse(input);
    const messages = request.messages.map(({ role, content }) => ({ role, content }));
    assert(Buffer.byteLength(JSON.stringify(messages)) <= this.maxInputBytes, 'input_byte_limit_exceeded');
    const body = stable(request); assert(Buffer.byteLength(body) <= 128_000, 'request_too_large');
    const digest = sha256(body); let row = this.journal.claim(requestId, body, digest);
    if (row.result) return strictJson(row.result);
    const owner = row.owner;
    try {
      assert(await this.rpc('getGenesisHash') === SOLANA_GENESIS, 'rpc_network_mismatch');
      if (!row.quote) {
        const response = await this.post(requestId, body);
        assert(response.status === 402, `quote_http_${response.status}`);
        const header = response.headers.get('PAYMENT-REQUIRED'); await response.body?.cancel();
        assert(header && header.length < 16_384, 'missing_payment_required');
        const required = strictJson(Buffer.from(header, 'base64').toString('utf8'));
        const { price } = this.validateQuote(required, digest);
        this.journal.reserve(requestId, owner, required, price); row = this.journal.get(requestId);
      }
      const required = strictJson(row.quote); this.validateQuote(required, digest, Boolean(row.payment));
      if (!row.payment) {
        const payload = await this.http.createPaymentPayload(required);
        assert(payload.x402Version === 2 && stable(payload.accepted) === stable(required.accepts[0]), 'sdk_changed_payment_requirements');
        const headers = this.http.encodePaymentSignatureHeader(payload);
        const payment = new Headers(headers).get('PAYMENT-SIGNATURE'); assert(payment, 'missing_payment_signature');
        this.validateQuote(required, digest);
        this.journal.signed(requestId, owner, payment); row = this.journal.get(requestId);
      }
      this.journal.check(requestId, owner);
      const response = await this.post(requestId, body, row.payment);
      if (response.status === 202) { await response.body?.cancel(); return { status: 'pending', requestId, requestSha256: digest }; }
      assert(response.status === 200, `paid_http_${response.status}_authorization_retained`);
      const result = strictJson(await bodyText(response));
      const verified = verifyComputeResult(result, { request, requestSha256: digest, quoteId: required.accepts[0].extra.quoteId, gatewayBuyer: this.gatewayBuyer, operators: this.operators });
      const settlement = await this.verifySettlement(response.headers.get('PAYMENT-RESPONSE'), row.payment);
      const complete = { status: 'completed', requestId, requestSha256: digest, amountMicroUsdc: row.amount, settlement,
        text: verified.text, result };
      this.journal.complete(requestId, owner, JSON.stringify(complete)); return complete;
    } finally { this.journal.release(requestId, owner); }
  }
  close() { this.journal.close(); }
}
