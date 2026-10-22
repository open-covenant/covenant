// Solana payments settled by an x402 facilitator over HTTP. The payment is
// verified here first and again by the facilitator, which adds its fee payer
// signature and broadcasts. The gateway journals the payer-signed transaction
// before /settle and reads every outcome from the chain: the fee payer's
// Ed25519 signature over the same message is deterministic, so a resubmission
// is the same transaction and a lost reply can never cost a second payment.
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
/** Transactions read per settle call; a longer search resumes on the next call. */
const READS_PER_SETTLE = 40;
const PAGES_PER_SETTLE = 20;
const MAX_UNRELATED = 50_000;

function digest(bytes) { return createHash('sha256').update(bytes).digest('hex'); }

/** Fee payer, recent blockhash and the payer's USDC source account of a payer-signed transaction. */
export function messageFacts(tx) {
  const message = getCompiledTransactionMessageDecoder().decode(tx.messageBytes);
  const accounts = message.staticAccounts.map(String);
  const transfer = message.instructions.find(ix => TOKEN_PROGRAMS.has(accounts[ix.programAddressIndex]));
  const source = transfer && accounts[transfer.accountIndices?.[0]];
  if (!source) throw new Rejected('payment has no token transfer');
  return { feePayer: accounts[0], blockhash: String(message.lifetimeToken), source };
}

export function createRemoteSvmRail({ config, rpc, facilitator, verifier, now = Date.now, log = () => {} }) {
  const { network, asset, payTo, feePayer, resourceUrl, secret } = config;
  const { pricing, requirements } = createQuoteRules({ config, now });
  const seal = value => createHmac('sha256', secret).update(canonical(value)).digest('hex');
  const submissions = new Map();
  const settling = new Set();
  /** Signatures the facilitator reported: a shortcut, never proof. */
  const reported = new Map();
  /** Signatures already read and found to carry another message. */
  const unrelated = new Set();

  async function assertNetwork() {
    if (await rpc('getGenesisHash', []) !== RAILS[network].genesis) throw new Error('RPC network mismatch');
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

  /** The finalized outcome of `signature` if it carries this message; null while undecided. */
  async function outcome(signature, message) {
    const status = (await rpc('getSignatureStatuses', [[signature], { searchTransactionHistory: true }]))?.value?.[0];
    if (status?.confirmationStatus !== 'finalized') return null;
    const found = await rpc('getTransaction', [signature, { encoding: 'base64', commitment: 'finalized', maxSupportedTransactionVersion: 0 }]);
    if (!found || !Array.isArray(found.transaction) || found.transaction[1] !== 'base64') return null;
    if (!Buffer.from(decode(found.transaction[0]).tx.messageBytes).equals(message)) return null;
    return status.err === null ? { status: 'finalized', transaction_id: signature } : { status: 'failed' };
  }

  /**
   * The signature of the landed transaction carrying this message. `null` only
   * when every transaction on the payer's account back to before the quote was
   * read and none matched; `undefined` when the search could not finish.
   */
  async function search(ev, message, facts, commitment, minContextSlot) {
    const notBefore = ev.terms.extra.expiresAtMs - 90_000 - SEARCH_MARGIN_MS;
    let before;
    let reads = 0;
    let unreadable = false;
    for (let pages = 0; pages < PAGES_PER_SETTLE; pages++) {
      const page = (await rpc('getSignaturesForAddress', [facts.source,
        { limit: 100, commitment, ...(minContextSlot ? { minContextSlot } : {}), ...(before ? { before } : {}) }])) ?? [];
      for (const candidate of page) {
        if (Number.isSafeInteger(candidate.blockTime) && candidate.blockTime * 1000 < notBefore) return unreadable ? undefined : null;
        if (unrelated.has(candidate.signature)) continue;
        if (++reads > READS_PER_SETTLE) return undefined;
        const found = await rpc('getTransaction', [candidate.signature, { encoding: 'base64', commitment, maxSupportedTransactionVersion: 0 }]);
        if (!found || !Array.isArray(found.transaction) || found.transaction[1] !== 'base64') { unreadable = true; continue; }
        if (Buffer.from(decode(found.transaction[0]).tx.messageBytes).equals(message)) return candidate.signature;
        if (unrelated.size >= MAX_UNRELATED) unrelated.clear();
        unrelated.add(candidate.signature);
      }
      if (page.length < 100) return unreadable ? undefined : null;
      before = page.at(-1).signature;
    }
    return undefined;
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

  function forget(authorizationId) {
    submissions.delete(authorizationId);
    reported.delete(authorizationId);
  }

  async function decide(ev, message, facts, terms) {
    const known = reported.get(ev.authorizationId);
    if (known) {
      const result = await outcome(known, message);
      if (result) return result;
    }
    const validity = await rpc('isBlockhashValid', [facts.blockhash, { commitment: 'finalized' }]);
    if (validity?.value) {
      // Resubmitting the same message is harmless, so it never waits on a search.
      submit(ev, terms);
      const signature = await search(ev, message, facts, 'confirmed');
      return (signature && await outcome(signature, message)) ?? { status: 'pending' };
    }
    // Expired at a finalized slot: any block that included it is finalized by
    // now, so a complete search at finalized from that slot is the answer.
    const signature = await search(ev, message, facts, 'finalized', validity?.context?.slot);
    if (signature === undefined) return { status: 'pending' };
    if (signature === null) return { status: 'failed' };
    return (await outcome(signature, message)) ?? { status: 'pending' };
  }

  return {
    network,
    assertNetwork,
    readiness() {
      const observedAtMs = now();
      if (!pricing.valid || !Number.isSafeInteger(observedAtMs) || observedAtMs < 0) throw new Error('payment pricing unavailable');
      return { schemaVersion: 1, observedAtMs, pricingValid: true, scheme: 'exact',
        network, asset, assetDecimals: 6, payTo, feePayer, resourceUrl, ...pricing.snapshot() };
    },
    async verify({ paymentPayload: payload, paymentRequirements: terms, resourceUrl: url }) {
      const expectedResource = requirements(terms, true);
      if (url !== expectedResource || payload?.resource?.url !== expectedResource || payload?.x402Version !== 2 || !isDeepStrictEqual(payload.accepted, terms)) throw new Rejected('payment payload does not match quote');
      const { bytes, tx } = decode(payload.payload?.transaction);
      if (messageFacts(tx).feePayer !== feePayer) throw new Rejected('payment names another fee payer');
      await assertNetwork();
      const local = await verifier.verify(payload, terms);
      if (!local.isValid || !local.payer) throw new Rejected(local.invalidReason ?? 'invalid payment');
      let remote;
      try { remote = await facilitator.verify(payload, terms); } catch (error) {
        if (error instanceof VerifyError) throw new Rejected(error.invalidReason ?? 'invalid payment');
        throw error;
      }
      if (!remote?.isValid || remote.payer !== local.payer) throw new Rejected(remote?.invalidReason ?? 'invalid payment');
      // Nothing is broadcast here. The signature does not exist until the
      // facilitator signs, so the authorization id stands in for it.
      const authorizationId = digest(bytes);
      const evidence = { payload, authorizationId, payer: local.payer, terms };
      return { payer: local.payer, authorization_id: authorizationId, transaction_id: authorizationId,
        encoded_transaction: JSON.stringify({ evidence, seal: seal(evidence) }) };
    },
    async settle({ payment, paymentRequirements: terms }) {
      // Recovery follows the sealed original quote, even after pricing changes.
      requirements(terms, false);
      const { ev, message, facts } = unseal(payment, terms);
      if (settling.has(ev.authorizationId)) return { status: 'pending' };
      settling.add(ev.authorizationId);
      try {
        await assertNetwork();
        const result = await decide(ev, message, facts, terms);
        if (result.status !== 'pending') forget(ev.authorizationId);
        return result;
      } finally {
        settling.delete(ev.authorizationId);
      }
    },
  };
}
