// Official x402 static SVM verification, with durable settlement owned by the
// Rust gateway. No call to the SDK's in-memory settlement cache can broadcast.
import { createHash, createHmac, timingSafeEqual } from 'node:crypto';
import { isDeepStrictEqual } from 'node:util';
import { getTransactionDecoder, getSignatureFromTransaction, assertIsFullySignedTransaction } from '@solana/kit';
import { ExactSvmScheme } from '@x402/svm/exact/facilitator';
import { createPricingPolicy, validPaymentAmount } from './pricing.mjs';

export const RAILS = Object.freeze({
  'solana:5eykt4UsFv8P8NJdTREpY1vzqKqZKvdp': {
    genesis: '5eykt4UsFv8P8NJdTREpY1vzqKqZKvdpKuc147dw2N9d',
    asset: 'EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v',
  },
  'solana:EtWTRABZaYq6iMfeYKouRu166VU2xqa1': {
    genesis: 'EtWTRABZaYq6iMfeYKouRu166VU2xqa1wcaWoxPkrZBG',
    asset: '4zMMC9srt5Ri5X14GAgXhaHii3GnPAEERYPJgZJDncDU',
  },
});
export class Rejected extends Error {}
export function canonical(value) {
  if (Array.isArray(value)) return `[${value.map(canonical).join(',')}]`;
  if (value !== null && typeof value === 'object') return `{${Object.keys(value).sort().map(k => `${JSON.stringify(k)}:${canonical(value[k])}`).join(',')}}`;
  return JSON.stringify(value);
}
function digest(bytes) { return createHash('sha256').update(bytes).digest('hex'); }
function decode(wire) {
  if (typeof wire !== 'string' || !/^[A-Za-z0-9+/]*={0,2}$/.test(wire)) throw new Rejected('invalid transaction');
  const bytes = Buffer.from(wire, 'base64');
  if (!bytes.length || bytes.length > 1232 || bytes.toString('base64') !== wire) throw new Rejected('invalid transaction encoding');
  return { bytes, tx: getTransactionDecoder().decode(bytes) };
}
export function createBridge({ signer, config, rpc, now = Date.now, verifier }) {
  const { network, asset, payTo, feePayer, resourceUrl, secret } = config;
  if (!RAILS[network] || asset !== RAILS[network].asset || secret.length < 32) throw new Error('invalid rail configuration');
  const pricing = createPricingPolicy(config);
  // The request-bound memo consumed 43,642 CU in a read-only mainnet simulation.
  // Bound sponsorship: <= 2 signatures, 60k CU and 1000 microlamports/CU.
  // This bounds fees to 10,060 lamports per admitted authorization.
  verifier ??= new ExactSvmScheme(signer, undefined, { enableSmartWalletVerification: false, maxRequiredSignatures: 2, maxComputeUnits: 60_000, maxPriorityFeeMicroLamports: 1_000 });
  function requirements(terms, fresh) {
    const extra = terms?.extra;
    if (terms?.scheme !== 'exact' || terms.network !== network || terms.asset !== asset || terms.payTo !== payTo || extra?.feePayer !== feePayer || !validPaymentAmount(terms.amount) || terms.maxTimeoutSeconds !== 90) throw new Rejected('payment rail or amount mismatch');
    if (fresh && !pricing.allows(terms.amount, now())) throw new Rejected('payment amount is not configured for admission');
    if (!/^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/.test(extra.quoteId) || !/^[0-9a-f]{64}$/.test(extra.requestSha256) || extra.memo !== `covenant.x402.v2:${extra.quoteId}:${extra.requestSha256}` || !Number.isSafeInteger(extra.expiresAtMs)) throw new Rejected('invalid quote binding');
    if (fresh && (now() > extra.expiresAtMs || extra.expiresAtMs > now() + 90_000)) throw new Rejected('quote expired');
  }
  function seal(value) { return createHmac('sha256', secret).update(canonical(value)).digest('hex'); }
  async function assertNetwork() {
    const genesis = await rpc('getGenesisHash', []);
    if (genesis !== RAILS[network].genesis) throw new Error('RPC network mismatch');
  }
  return {
    async verify({ paymentPayload: payload, paymentRequirements: terms, resourceUrl: url }) {
      requirements(terms, true);
      if (url !== resourceUrl || payload?.resource?.url !== resourceUrl || payload?.x402Version !== 2 || !isDeepStrictEqual(payload.accepted, terms)) throw new Rejected('payment payload does not match quote');
      await assertNetwork();
      const { bytes } = decode(payload.payload?.transaction);
      const verified = await verifier.verify(payload, terms);
      if (!verified.isValid || !verified.payer) throw new Rejected(verified.invalidReason ?? 'invalid payment');
      // Signing is local. The Rust journal must persist these exact bytes before
      // /settle may be invoked, so a lost send response cannot create a new tx.
      const signed = await signer.signTransaction(payload.payload.transaction, feePayer, network);
      const { tx } = decode(signed);
      assertIsFullySignedTransaction(tx);
      const authorizationId = digest(bytes);
      const transactionId = getSignatureFromTransaction(tx);
      const evidence = { transaction: signed, authorizationId, transactionId, payer: verified.payer, terms };
      return { payer: verified.payer, authorization_id: authorizationId, transaction_id: transactionId, encoded_transaction: JSON.stringify({ evidence, seal: seal(evidence) }) };
    },
    async settle({ payment, paymentRequirements: terms }) {
      // Recovery follows the sealed original quote, even after its tariff has
      // expired/rotated or optional pricing configuration becomes unreadable.
      requirements(terms, false);
      let saved;
      try { saved = JSON.parse(payment.encoded_transaction); } catch { throw new Rejected('invalid stored authorization'); }
      const expectedSeal = seal(saved.evidence);
      if (typeof saved.seal !== 'string' || saved.seal.length !== expectedSeal.length || !timingSafeEqual(Buffer.from(saved.seal), Buffer.from(expectedSeal))) throw new Rejected('stored authorization changed');
      const ev = saved.evidence;
      if (!isDeepStrictEqual(ev.terms, terms) || payment.transaction_id !== ev.transactionId || payment.authorization_id !== ev.authorizationId || payment.payer !== ev.payer) throw new Rejected('stored payment binding changed');
      const { tx } = decode(ev.transaction);
      assertIsFullySignedTransaction(tx);
      if (getSignatureFromTransaction(tx) !== payment.transaction_id) throw new Rejected('signature changed');
      await assertNetwork();
      const signature = payment.transaction_id;
      const statuses = await rpc('getSignatureStatuses', [[signature], { searchTransactionHistory: true }]);
      const status = statuses?.value?.[0];
      if (status?.confirmationStatus === 'finalized') {
        if (status.err !== null) return { status: 'failed' };
        const transaction = await rpc('getTransaction', [signature, { encoding: 'base64', commitment: 'finalized', maxSupportedTransactionVersion: 0 }]);
        if (!transaction) return { status: 'pending' };
        if (transaction.meta?.err !== null || !Array.isArray(transaction.transaction) || transaction.transaction[0] !== ev.transaction || transaction.transaction[1] !== 'base64') throw new Error('finalized transaction readback mismatch');
        return { status: 'finalized', transaction_id: signature };
      }
      if (status) return { status: 'pending' };
      // Re-broadcast the original signed transaction only. Expiry, RPC errors,
      // and lost responses are uncertain, never permission to create a new one.
      try {
        const sent = await rpc('sendTransaction', [ev.transaction, { encoding: 'base64', skipPreflight: false, preflightCommitment: 'confirmed', maxRetries: 0 }]);
        if (sent !== signature) throw new Error('RPC returned another signature');
      } catch { return { status: 'pending' }; }
      return { status: 'pending' };
    },
    assertNetwork,
  };
}
