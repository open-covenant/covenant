// Official x402 static SVM verification, with durable settlement owned by the
// Rust gateway. The SDK's settlement path never runs here; the stored bytes are
// broadcast directly or by a facilitator holding the same fee payer key.
import { createHash, createHmac, timingSafeEqual } from 'node:crypto';
import { isDeepStrictEqual } from 'node:util';
import { getCompiledTransactionMessageDecoder, getTransactionDecoder, getSignatureFromTransaction, assertIsFullySignedTransaction } from '@solana/kit';
import { ExactSvmScheme } from '@x402/svm/exact/facilitator';
import { batchPricingTotal, createPricingPolicy, validPaymentAmount } from './pricing.mjs';
import { buildRefund, MEMO_PROGRAM, refundMarker } from './refunds.mjs';
import { findAssociatedTokenPda, TOKEN_PROGRAM_ADDRESS } from '@solana-program/token';

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
// v3 memos are 32 hex characters, which fit the official client's 20k compute units.
export function shortMemo(quoteId, requestSha256) { return digest(`covenant.x402.v3:${quoteId}:${requestSha256}`).slice(0, 32); }
// Quotes issued before v3 keep their v2 memo until they settle.
function validMemo(extra) {
  return extra.memo === shortMemo(extra.quoteId, extra.requestSha256) || extra.memo === `covenant.x402.v2:${extra.quoteId}:${extra.requestSha256}`;
}
function decode(wire) {
  if (typeof wire !== 'string' || !/^[A-Za-z0-9+/]*={0,2}$/.test(wire)) throw new Rejected('invalid transaction');
  const bytes = Buffer.from(wire, 'base64');
  if (!bytes.length || bytes.length > 1232 || bytes.toString('base64') !== wire) throw new Rejected('invalid transaction encoding');
  return { bytes, tx: getTransactionDecoder().decode(bytes) };
}
/**
 * Whether two wire transactions are the same payment: the same message and
 * the same fee payer signature, which is the transaction id. Any other
 * signer may sign the same message again with a different signature, and
 * the network accepts either.
 */
function sameTransaction(a, b) {
  try {
    const [x, y] = [decode(a).tx, decode(b).tx];
    return Buffer.from(x.messageBytes).equals(Buffer.from(y.messageBytes)) && getSignatureFromTransaction(x) === getSignatureFromTransaction(y);
  } catch { return false; }
}
/** One facilitator submission per payment at a time, repeated at most this often while the signature is unseen. */
export const RESUBMIT_MS = 20_000;
/** How long a facilitator call may run before the same bytes are also sent from here. */
export const FACILITATOR_GRACE_MS = 4_000;
/**
 * A blockhash lives about 60 to 90 seconds, so no transaction signed before
 * a quote expired is checked for expiry until this long after it.
 */
export const EXPIRY_MARGIN_MS = 180_000;
/** The token program's TransferChecked instruction. */
const TRANSFER_CHECKED = 12;

/** The paid job kinds a gateway can sell, each at `/v1/jobs/<kind>`. */
export const JOB_KINDS = Object.freeze(['research', 'knowledge', 'agent', 'fix']);

/**
 * Job prices by resource URL, for the rail that sells paid jobs. Each URL is
 * an https `/v1/jobs/<kind>` path and each price a positive atomic amount.
 */
export function jobPriceTable(prices) {
  if (prices === null || typeof prices !== 'object' || Array.isArray(prices)) throw new Error('invalid job prices');
  const table = new Map(Object.entries(prices));
  // An empty table sells nothing new while open jobs settle and refund.
  if (table.size > 8) throw new Error('invalid job prices');
  for (const [url, amount] of table) {
    let parsed;
    try { parsed = new URL(url); } catch { throw new Error('invalid job prices'); }
    if (parsed.protocol !== 'https:' || parsed.username || parsed.password || parsed.search || parsed.hash
      || parsed.href !== url || !JOB_KINDS.some(kind => parsed.pathname === `/v1/jobs/${kind}`) || !validPaymentAmount(amount)) throw new Error('invalid job prices');
  }
  return table;
}

export function createBridge({ signer, config, rpc, now = Date.now, verifier, facilitator, refundSigners, log = () => {} }) {
  const { network, asset, payTo, feePayer, resourceUrl, secret } = config;
  if (!RAILS[network] || asset !== RAILS[network].asset || secret.length < 32) throw new Error('invalid rail configuration');
  // A jobs rail sells fixed-price jobs to its own merchant and never inference.
  // It refunds a failed job from that merchant, so it holds the merchant's key.
  const jobs = config.jobPrices === undefined ? null : jobPriceTable(config.jobPrices);
  if (jobs && (refundSigners?.merchant?.address !== payTo || refundSigners?.feePayer?.address !== feePayer)) throw new Error('jobs rail requires the merchant and fee payer keys');
  const pricing = jobs ? null : createPricingPolicy(config);
  const batchResourceUrl = jobs ? null : new URL('/v1/inference/batches', resourceUrl).href;
  // A v3 payment from the official client uses about 13k of its 20k CU; the
  // 60k ceiling admits v2 memos (43,642 CU) signed by earlier Covenant clients.
  // Bound sponsorship: <= 2 signatures, 60k CU and 1000 microlamports/CU.
  // This bounds fees to 10,060 lamports per admitted authorization.
  verifier ??= new ExactSvmScheme(signer, undefined, { enableSmartWalletVerification: false, maxRequiredSignatures: 2, maxComputeUnits: 60_000, maxPriorityFeeMicroLamports: 1_000 });
  /** The resource a payment for these terms must name. `url` is the one a fresh job payment claims. */
  function requirements(terms, fresh, url) {
    const extra = terms?.extra;
    if (terms?.scheme !== 'exact' || terms.network !== network || terms.asset !== asset || terms.payTo !== payTo || extra?.feePayer !== feePayer || !validPaymentAmount(terms.amount) || terms.maxTimeoutSeconds !== 90) throw new Rejected('payment rail or amount mismatch');
    const batch = Object.hasOwn(extra, 'batchPricing');
    if (batch && (jobs || batchPricingTotal(extra.batchPricing) !== BigInt(terms.amount))) throw new Rejected('invalid batch pricing binding');
    const allowed = jobs ? jobs.get(url) === terms.amount
      : batch ? pricing.allowsBatch(extra.batchPricing, terms.amount, now()) : pricing.allows(terms.amount, now());
    if (fresh && !allowed) throw new Rejected('payment amount is not configured for admission');
    if (!/^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/.test(extra.quoteId) || !/^[0-9a-f]{64}$/.test(extra.requestSha256) || !validMemo(extra) || !Number.isSafeInteger(extra.expiresAtMs)) throw new Rejected('invalid quote binding');
    if (fresh && (now() > extra.expiresAtMs || extra.expiresAtMs > now() + 90_000)) throw new Rejected('quote expired');
    return jobs ? url : batch ? batchResourceUrl : resourceUrl;
  }
  function seal(value) { return createHmac('sha256', secret).update(canonical(value)).digest('hex'); }
  /** The evidence of a record this process sealed, refused if anything in it changed. */
  function unseal(encoded, what) {
    let saved;
    try { saved = JSON.parse(encoded); } catch { throw new Rejected(`invalid stored ${what}`); }
    if (saved?.evidence === null || typeof saved?.evidence !== 'object') throw new Rejected(`invalid stored ${what}`);
    const expectedSeal = seal(saved.evidence);
    if (typeof saved.seal !== 'string' || saved.seal.length !== expectedSeal.length || !timingSafeEqual(Buffer.from(saved.seal), Buffer.from(expectedSeal))) throw new Rejected(`stored ${what} changed`);
    return saved.evidence;
  }
  /**
   * A verified payment as the gateway stored it. Recovery follows the sealed
   * original quote, even after its tariff has expired/rotated or optional
   * pricing configuration becomes unreadable.
   */
  function storedPayment(payment, terms) {
    requirements(terms, false);
    const ev = unseal(payment?.encoded_transaction, 'authorization');
    if (!isDeepStrictEqual(ev.terms, terms) || payment.transaction_id !== ev.transactionId || payment.authorization_id !== ev.authorizationId || payment.payer !== ev.payer) throw new Rejected('stored payment binding changed');
    const { tx } = decode(ev.transaction);
    assertIsFullySignedTransaction(tx);
    if (getSignatureFromTransaction(tx) !== payment.transaction_id) throw new Rejected('signature changed');
    return ev;
  }
  const submissions = new Map();
  /**
   * Whether to leave the broadcast to the facilitator for now: during a young
   * call, or for a while after one returned. A slow, failed or declined call
   * hands it back, so a payment never waits on the facilitator to expire.
   */
  function submit(ev, terms) {
    const last = submissions.get(ev.transactionId);
    if (last?.failed) return false;
    if (last?.running) return now() - last.at < FACILITATOR_GRACE_MS;
    if (last && now() - last.at < RESUBMIT_MS) return true;
    const attempt = { running: true, at: now(), failed: false };
    submissions.set(ev.transactionId, attempt);
    (async () => {
      try {
        const result = await facilitator.settle(ev.payload, terms);
        if (!result?.success) { attempt.failed = true; log(`facilitator settle declined: ${result?.errorReason ?? 'no reason'}`); }
        else if (result.transaction !== ev.transactionId) { attempt.failed = true; log('facilitator settled another signature'); }
      } catch {
        attempt.failed = true;
        log('facilitator settle unavailable; sending directly');
      } finally { attempt.running = false; }
    })();
    return true;
  }
  async function assertNetwork() {
    const genesis = await rpc('getGenesisHash', []);
    if (genesis !== RAILS[network].genesis) throw new Error('RPC network mismatch');
  }
  /**
   * Whether a transaction never landed and never can. Its blockhash must have
   * aged out of the newest bank (so it is old, not merely newer than the
   * finalized one) and of the finalized bank, which puts every block that
   * could hold it behind finality. Only then is a missing status conclusive.
   */
  async function expired(wire, signature, issuedBefore) {
    if (now() < issuedBefore + EXPIRY_MARGIN_MS) return false;
    const { lifetimeToken } = getCompiledTransactionMessageDecoder().decode(decode(wire).tx.messageBytes);
    const processed = await rpc('isBlockhashValid', [lifetimeToken, { commitment: 'processed' }]);
    if (processed?.value !== false) return false;
    const finalized = await rpc('isBlockhashValid', [lifetimeToken, { commitment: 'finalized' }]);
    if (finalized?.value !== false) return false;
    // Each later read must come from a node at least as far along as the
    // finalized one, or a lagging node behind a load balancer could miss it.
    const floor = finalized.context?.slot;
    if (!Number.isSafeInteger(floor) || !(processed.context?.slot >= floor)) return false;
    const statuses = await rpc('getSignatureStatuses', [[signature], { searchTransactionHistory: true }]);
    return statuses?.value?.[0] == null && statuses?.context?.slot >= floor;
  }
  return {
    readiness() {
      const observedAtMs = now();
      if (!(jobs || pricing.valid) || !Number.isSafeInteger(observedAtMs) || observedAtMs < 0) throw new Error('payment pricing unavailable');
      // A read of loaded policy only: no verifier, RPC, signature or broadcast.
      const base = { schemaVersion: 1, observedAtMs, pricingValid: true, scheme: 'exact', network, asset, assetDecimals: 6, payTo, feePayer };
      return jobs ? { ...base, jobPrices: Object.fromEntries(jobs) } : { ...base, resourceUrl, ...pricing.snapshot() };
    },
    async verify({ paymentPayload: payload, paymentRequirements: terms, resourceUrl: url }) {
      const expectedResource = requirements(terms, true, url);
      if (url !== expectedResource || payload?.resource?.url !== expectedResource || payload?.x402Version !== 2 || !isDeepStrictEqual(payload.accepted, terms)) throw new Rejected('payment payload does not match quote');
      await assertNetwork();
      const { bytes } = decode(payload.payload?.transaction);
      const verified = await verifier.verify(payload, terms);
      if (!verified.isValid || !verified.payer) throw new Rejected(verified.invalidReason ?? 'invalid payment');
      if (jobs) await paymentTransfer(payload.payload.transaction, verified.payer);
      // Signing is local. The Rust journal must persist these exact bytes before
      // /settle may be invoked, so a lost send response cannot create a new tx.
      const signed = await signer.signTransaction(payload.payload.transaction, feePayer, network);
      const { tx } = decode(signed);
      assertIsFullySignedTransaction(tx);
      const authorizationId = digest(bytes);
      const transactionId = getSignatureFromTransaction(tx);
      // The original payload lets a facilitator broadcast this same transaction.
      const evidence = { transaction: signed, authorizationId, transactionId, payer: verified.payer, terms, ...(facilitator ? { payload } : {}) };
      return { payer: verified.payer, authorization_id: authorizationId, transaction_id: transactionId, encoded_transaction: JSON.stringify({ evidence, seal: seal(evidence) }) };
    },
    async settle({ payment, paymentRequirements: terms }) {
      const ev = storedPayment(payment, terms);
      await assertNetwork();
      const signature = payment.transaction_id;
      const statuses = await rpc('getSignatureStatuses', [[signature], { searchTransactionHistory: true }]);
      const status = statuses?.value?.[0];
      if (status?.confirmationStatus === 'finalized') {
        submissions.delete(signature);
        if (status.err !== null) return { status: 'failed' };
        const transaction = await rpc('getTransaction', [signature, { encoding: 'base64', commitment: 'finalized', maxSupportedTransactionVersion: 0 }]);
        if (!transaction) return { status: 'pending' };
        if (transaction.meta?.err !== null || !Array.isArray(transaction.transaction) || transaction.transaction[1] !== 'base64' || !sameTransaction(transaction.transaction[0], ev.transaction)) throw new Error('finalized transaction readback mismatch');
        return { status: 'finalized', transaction_id: signature };
      }
      if (status) return { status: 'pending' };
      // A job waits on its payment before any work starts, so a payment that
      // can no longer land must end as unpaid rather than wait forever.
      if (jobs && await expired(ev.transaction, signature, terms.extra.expiresAtMs)) return { status: 'failed' };
      // A facilitator signs the same message with the same fee payer key, and
      // Ed25519 signatures are deterministic, so it broadcasts exactly these
      // bytes. If it is unreachable or declines, they are sent from here.
      if (facilitator && ev.payload && submit(ev, terms)) return { status: 'pending' };
      // Re-broadcast the original signed transaction only. Expiry, RPC errors,
      // and lost responses are uncertain, never permission to create a new one.
      try {
        const sent = await rpc('sendTransaction', [ev.transaction, { encoding: 'base64', skipPreflight: false, preflightCommitment: 'confirmed', maxRetries: 0 }]);
        if (sent !== signature) throw new Error('RPC returned another signature');
      } catch { return { status: 'pending' }; }
      return { status: 'pending' };
    },
    ...(jobs ? { refund, refundStatus } : {}),
    assertNetwork,
  };
  /**
   * The payment's single USDC transfer, which must come from the payer's own
   * account and go to the merchant's. A delegate or another owner's account
   * is refused, so a refund always returns to the account that paid.
   */
  async function paymentTransfer(wire, payer) {
    const message = getCompiledTransactionMessageDecoder().decode(decode(wire).tx.messageBytes);
    const keys = message.staticAccounts;
    const transfers = message.instructions.filter(ix => keys[ix.programAddressIndex] === TOKEN_PROGRAM_ADDRESS);
    if (transfers.length !== 1) throw new Rejected('a job payment makes one token transfer');
    const data = Buffer.from(transfers[0].data ?? []);
    const accounts = (transfers[0].accountIndices ?? []).map(index => keys[index]);
    if (data.length !== 10 || data[0] !== TRANSFER_CHECKED || accounts.length !== 4 || accounts.includes(undefined)) throw new Rejected('a job payment is a checked token transfer');
    const [source, mint, destination, authority] = accounts;
    const [payerAccount] = await findAssociatedTokenPda({ owner: payer, mint: asset, tokenProgram: TOKEN_PROGRAM_ADDRESS });
    const [merchantAccount] = await findAssociatedTokenPda({ owner: payTo, mint: asset, tokenProgram: TOKEN_PROGRAM_ADDRESS });
    if (authority !== payer || source !== payerAccount || mint !== asset || destination !== merchantAccount) throw new Rejected("a job is paid from the payer's own USDC account");
    return { source, destination, amount: data.readBigUInt64LE(1) };
  }
  /** The payment as it landed: these exact bytes, finalized and successful, for the quoted amount. */
  async function landedTransfer(ev, terms) {
    const status = (await rpc('getSignatureStatuses', [[ev.transactionId], { searchTransactionHistory: true }]))?.value?.[0];
    if (status?.confirmationStatus !== 'finalized' || status.err !== null) throw new Rejected('only a finalized payment is refunded');
    const landed = await rpc('getTransaction', [ev.transactionId, { encoding: 'base64', commitment: 'finalized', maxSupportedTransactionVersion: 0 }]);
    if (!landed || landed.meta?.err !== null || !sameTransaction(landed.transaction?.[0], ev.transaction)) throw new Rejected('payment readback does not match');
    const transfer = await paymentTransfer(ev.transaction, ev.payer);
    if (transfer.amount !== BigInt(terms.amount)) throw new Rejected('payment readback does not match');
    return transfer;
  }
  /**
   * The refund that claimed this payment's marker, once one has. The marker
   * and the money move in one transaction, so a claimed marker proves the
   * refund; its id is the oldest successful transaction this fee payer sent
   * that names the marker, or "unrecorded" when history cannot show it.
   */
  async function claimedRefund(marker) {
    const account = (await rpc('getAccountInfo', [marker, { encoding: 'base64', commitment: 'finalized' }]))?.value;
    if (account?.owner !== MEMO_PROGRAM) return null;
    try {
      const history = await rpc('getSignaturesForAddress', [marker, { limit: 1000, commitment: 'finalized' }]);
      for (const entry of [...(history ?? [])].reverse()) {
        if (entry.err !== null) continue;
        const landed = await rpc('getTransaction', [entry.signature, { encoding: 'base64', commitment: 'finalized', maxSupportedTransactionVersion: 0 }]);
        const keys = getCompiledTransactionMessageDecoder().decode(decode(landed?.transaction?.[0]).tx.messageBytes).staticAccounts;
        if (keys[0] === feePayer && keys.includes(marker)) return entry.signature;
      }
    } catch { /* The refund happened; only its id is unknown. */ }
    return 'unrecorded';
  }
  /**
   * A signed refund of a finalized job payment, sealed for the gateway to
   * store before anything is broadcast. The payment is read back from chain,
   * so the amount and accounts come from what landed, never from the caller.
   * A payment already refunded reports that refund instead.
   */
  async function refund({ payment, paymentRequirements: terms }) {
    const ev = storedPayment(payment, terms);
    await assertNetwork();
    const transfer = await landedTransfer(ev, terms);
    const marker = await refundMarker(payTo, ev.transactionId);
    const claimed = await claimedRefund(marker);
    if (claimed) return { status: 'refunded', transaction_id: claimed };
    const latest = (await rpc('getLatestBlockhash', [{ commitment: 'confirmed' }]))?.value;
    const markerLamports = await rpc('getMinimumBalanceForRentExemption', [1]);
    if (typeof latest?.blockhash !== 'string' || !Number.isSafeInteger(latest.lastValidBlockHeight) || !Number.isSafeInteger(markerLamports)) throw new Error('RPC unavailable');
    const built = await buildRefund({ ...refundSigners, mint: asset, source: transfer.destination, destination: transfer.source,
      amount: transfer.amount, quoteId: terms.extra.quoteId, paymentSignature: ev.transactionId, markerLamports,
      blockhash: latest.blockhash, lastValidBlockHeight: latest.lastValidBlockHeight });
    const evidence = { kind: 'refund', transaction: built.transaction, signature: built.signature, marker,
      lastValidBlockHeight: latest.lastValidBlockHeight, paymentTransactionId: ev.transactionId };
    return { status: 'built', transaction_id: built.signature, encoded_refund: JSON.stringify({ evidence, seal: seal(evidence) }) };
  }
  /**
   * Where a stored refund stands. `expired` and `failed` mean this refund
   * moved nothing and never can, so the gateway may build another; the
   * marker stops that one from paying twice if this verdict is ever wrong.
   */
  async function refundStatus({ payment, paymentRequirements: terms, refund: encoded }) {
    const ev = storedPayment(payment, terms);
    const saved = unseal(encoded, 'refund');
    if (saved.kind !== 'refund' || saved.paymentTransactionId !== ev.transactionId) throw new Rejected('stored refund binding changed');
    const { tx } = decode(saved.transaction);
    if (getSignatureFromTransaction(tx) !== saved.signature) throw new Rejected('signature changed');
    await assertNetwork();
    // Finality first, and a status read from a node at least that far along:
    // past the refund's last valid height, a missing status is conclusive.
    const finalized = await rpc('getEpochInfo', [{ commitment: 'finalized' }]);
    const statuses = await rpc('getSignatureStatuses', [[saved.signature], { searchTransactionHistory: true }]);
    const status = statuses?.value?.[0];
    if (status?.confirmationStatus === 'finalized' && status.err === null) return { status: 'finalized', transaction_id: saved.signature };
    if (status && status.confirmationStatus !== 'finalized') return { status: 'pending' };
    // Another refund of this payment may have landed (one built before a
    // restart, say); the marker shows it, and this one can never land.
    const claimed = await claimedRefund(saved.marker);
    if (claimed) return { status: 'finalized', transaction_id: claimed };
    if (status) return { status: 'failed' };
    if (Number.isSafeInteger(finalized?.blockHeight) && finalized.blockHeight > saved.lastValidBlockHeight
      && statuses?.context?.slot >= finalized.absoluteSlot) return { status: 'expired' };
    try {
      await rpc('sendTransaction', [saved.transaction, { encoding: 'base64', skipPreflight: false, preflightCommitment: 'confirmed', maxRetries: 0 }]);
    } catch {
      // Usually an empty merchant account or a closed payer account. The
      // refund stays owed and is tried again.
      log('refund broadcast refused; it stays owed and will be retried');
    }
    return { status: 'pending' };
  }
}
