// Solana payments settled by an x402 facilitator over HTTP. The facilitator
// adds its fee payer signature and broadcasts; the gateway journals the
// payer-signed transaction before /settle and reads the outcome from the chain.
// A facilitator reply is never trusted: the fee payer's Ed25519 signature over
// the same message is deterministic, so a resubmission is the same transaction.
import { createHash, createHmac, timingSafeEqual } from 'node:crypto';
import { isDeepStrictEqual } from 'node:util';
import { getCompiledTransactionMessageDecoder } from '@solana/kit';
import { VerifyError } from '@x402/core/types';
import { decode, createQuoteRules, canonical, RAILS, Rejected } from './bridge.mjs';

const TOKEN_PROGRAMS = new Set(['TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA', 'TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb']);
/** One facilitator submission per authorization at a time, repeated at most this often while unseen on chain. */
export const RESUBMIT_MS = 20_000;
/** How far back a landed payment is searched for, beyond the quote's own window. */
const SEARCH_MARGIN_MS = 5 * 60_000;
const SUPPORTED_TTL_MS = 60_000;
/** A facilitator restart must not stop quoting: the last good /supported holds this long. */
const SUPPORTED_GRACE_MS = 10 * 60_000;
const MAX_UNRELATED = 50_000;
const MAX_SEARCH_PAGES = 20;

function digest(bytes) { return createHash('sha256').update(bytes).digest('hex'); }

/** Fee payer, recent blockhash and USDC destination of a payer-signed transaction. */
export function messageFacts(tx) {
  const message = getCompiledTransactionMessageDecoder().decode(tx.messageBytes);
  const accounts = message.staticAccounts.map(String);
  const transfer = message.instructions.find(ix => TOKEN_PROGRAMS.has(accounts[ix.programAddressIndex]));
  const destination = transfer && accounts[transfer.accountIndices?.[2]];
  if (!destination) throw new Rejected('payment has no token transfer');
  return { feePayer: accounts[0], blockhash: String(message.lifetimeToken), destination };
}

export function createRemoteSvmRail({ config, rpc, facilitator, now = Date.now, log = () => {} }) {
  const { network, asset, payTo, feePayer, resourceUrl, secret } = config;
  const { pricing, requirements } = createQuoteRules({ config, now });
  const seal = value => createHmac('sha256', secret).update(canonical(value)).digest('hex');
  const submissions = new Map();
  /** Signatures the facilitator reported, a shortcut past the chain search. */
  const reported = new Map();
  /** Signatures already read and found to be other transactions. */
  const unrelated = new Set();
  let supported = null;

  async function assertNetwork() {
    if (await rpc('getGenesisHash', []) !== RAILS[network].genesis) throw new Error('RPC network mismatch');
  }
  // Quotes name the facilitator's fee payer; one it no longer signs with would
  // produce payments it refuses, so readiness follows its /supported.
  async function assertFacilitator() {
    if (supported && now() - supported.atMs < SUPPORTED_TTL_MS) return;
    let kinds;
    try {
      kinds = (await facilitator.getSupported())?.kinds ?? [];
    } catch (error) {
      if (supported && now() - supported.atMs < SUPPORTED_GRACE_MS) return;
      throw error;
    }
    const kind = kinds.find(k => k.x402Version === 2 && k.scheme === 'exact' && k.network === network);
    if (kind?.extra?.feePayer !== feePayer) {
      supported = null;
      throw new Error('facilitator fee payer mismatch');
    }
    supported = { atMs: now() };
  }

  function unseal(payment, terms) {
    let saved;
    try { saved = JSON.parse(payment?.encoded_transaction); } catch { throw new Rejected('invalid stored authorization'); }
    if (typeof saved?.seal !== 'string' || typeof saved.evidence !== 'object' || saved.evidence === null) throw new Rejected('stored authorization changed');
    const expected = seal(saved.evidence);
    if (saved.seal.length !== expected.length || !timingSafeEqual(Buffer.from(saved.seal), Buffer.from(expected))) throw new Rejected('stored authorization changed');
    const ev = saved.evidence;
    if (!isDeepStrictEqual(ev.terms, terms) || payment.authorization_id !== ev.authorizationId || payment.transaction_id !== ev.authorizationId || payment.payer !== ev.payer) throw new Rejected('stored payment binding changed');
    const { bytes, tx } = decode(ev.payload?.payload?.transaction);
    if (digest(bytes) !== ev.authorizationId) throw new Rejected('stored payment binding changed');
    return { ev, message: Buffer.from(tx.messageBytes), facts: messageFacts(tx) };
  }

  /**
   * The landed transaction carrying this exact message: `null` when the search
   * reached back past the quote without finding it, `undefined` when it could
   * not reach that far, which must never be read as "not landed".
   */
  async function landed(ev, message, facts) {
    const known = reported.get(ev.authorizationId);
    if (known) return (await match(known, message)) ?? null;
    const notBefore = ev.terms.extra.expiresAtMs - 90_000 - SEARCH_MARGIN_MS;
    let before;
    for (let page = 0; page < MAX_SEARCH_PAGES; page++) {
      const batch = (await rpc('getSignaturesForAddress', [facts.destination, { limit: 100, commitment: 'confirmed', ...(before ? { before } : {}) }])) ?? [];
      for (const candidate of batch) {
        if (Number.isSafeInteger(candidate.blockTime) && candidate.blockTime * 1000 < notBefore) return null;
        if (unrelated.has(candidate.signature)) continue;
        const found = await match(candidate.signature, message);
        if (found) return found;
      }
      if (batch.length < 100) return null;
      before = batch.at(-1).signature;
    }
    return undefined;
  }
  async function match(signature, message) {
    const found = await rpc('getTransaction', [signature, { encoding: 'base64', commitment: 'confirmed', maxSupportedTransactionVersion: 0 }]);
    if (!found || !Array.isArray(found.transaction) || found.transaction[1] !== 'base64') return null;
    const { tx } = decode(found.transaction[0]);
    if (!Buffer.from(tx.messageBytes).equals(message)) {
      if (unrelated.size >= MAX_UNRELATED) unrelated.clear();
      unrelated.add(signature);
      return null;
    }
    return { signature, failed: found.meta?.err !== null };
  }

  function submit(ev, terms) {
    const last = submissions.get(ev.authorizationId);
    if (last && (last.running || now() - last.at < RESUBMIT_MS)) return;
    const attempt = { running: true, at: now() };
    submissions.set(ev.authorizationId, attempt);
    (async () => {
      try {
        const result = await facilitator.settle(ev.payload, terms);
        if (result?.success && typeof result.transaction === 'string') reported.set(ev.authorizationId, result.transaction);
        else log(`facilitator settle declined: ${result?.errorReason ?? 'no reason'}`);
      } catch { /* the next settle reads the chain */ } finally { attempt.running = false; }
    })();
  }

  return {
    network,
    assertNetwork,
    async readiness() {
      const observedAtMs = now();
      if (!pricing.valid || !Number.isSafeInteger(observedAtMs) || observedAtMs < 0) throw new Error('payment pricing unavailable');
      await assertFacilitator();
      return { schemaVersion: 1, observedAtMs, pricingValid: true, scheme: 'exact',
        network, asset, assetDecimals: 6, payTo, feePayer, resourceUrl, ...pricing.snapshot() };
    },
    async verify({ paymentPayload: payload, paymentRequirements: terms, resourceUrl: url }) {
      const expectedResource = requirements(terms, true);
      if (url !== expectedResource || payload?.resource?.url !== expectedResource || payload?.x402Version !== 2 || !isDeepStrictEqual(payload.accepted, terms)) throw new Rejected('payment payload does not match quote');
      const { bytes, tx } = decode(payload.payload?.transaction);
      if (messageFacts(tx).feePayer !== feePayer) throw new Rejected('payment names another fee payer');
      await assertNetwork();
      let verified;
      try { verified = await facilitator.verify(payload, terms); } catch (error) {
        if (error instanceof VerifyError) throw new Rejected(error.invalidReason ?? 'invalid payment');
        throw error;
      }
      if (!verified?.isValid || typeof verified.payer !== 'string') throw new Rejected(verified?.invalidReason ?? 'invalid payment');
      // Nothing is broadcast here. The transaction id is unknown until the
      // facilitator signs, so the authorization id stands in for it until then.
      const authorizationId = digest(bytes);
      const evidence = { payload, authorizationId, payer: verified.payer, terms };
      return { payer: verified.payer, authorization_id: authorizationId, transaction_id: authorizationId,
        encoded_transaction: JSON.stringify({ evidence, seal: seal(evidence) }) };
    },
    async settle({ payment, paymentRequirements: terms }) {
      // Recovery follows the sealed original quote, even after pricing changes.
      requirements(terms, false);
      const { ev, message, facts } = unseal(payment, terms);
      await assertNetwork();
      const found = await landed(ev, message, facts);
      if (found) {
        if (found.failed) return { status: 'failed' };
        const status = (await rpc('getSignatureStatuses', [[found.signature], { searchTransactionHistory: true }]))?.value?.[0];
        if (status?.confirmationStatus !== 'finalized') return { status: 'pending' };
        submissions.delete(ev.authorizationId);
        reported.delete(ev.authorizationId);
        return { status: 'finalized', transaction_id: found.signature };
      }
      if (found === undefined) return { status: 'pending' };
      // Unseen and its blockhash expired: no block can include it any more. A
      // transaction that landed before expiry is far older than the search lag.
      if (!(await rpc('isBlockhashValid', [facts.blockhash, { commitment: 'processed' }]))?.value) {
        if ((await landed(ev, message, facts)) !== null) return { status: 'pending' };
        submissions.delete(ev.authorizationId);
        return { status: 'failed' };
      }
      submit(ev, terms);
      return { status: 'pending' };
    },
  };
}
