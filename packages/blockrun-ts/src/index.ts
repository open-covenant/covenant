/**
 * @covenant-org/blockrun — a trust receipt for every BlockRun call.
 *
 * Wrap your fetch with {@link withCovenantReceipts} and each paid BlockRun
 * exchange emits a {@link CallReceipt} binding the requested model, the model
 * actually served, the input/output hashes, and the settlement — the same
 * receipt the Covenant daemon produces, verifiable with {@link verifyReceipt}.
 */
export {
  buildReceipt,
  canonicalSha256Hex,
  receiptDigest,
  verdictFor,
  verifyReceipt,
  PROVIDER,
  VERDICT_DELIVERED,
  VERDICT_SUBSTITUTED,
  VERDICT_UNVERIFIED,
} from "./receipt.js";
export type {
  CallReceipt,
  PaymentInfo,
  RoutingClaim,
  Verdict,
  VerifyResult,
} from "./receipt.js";
export { withCovenantReceipts } from "./decorator.js";
export type { FetchLike, OnReceipt, WrapOptions } from "./decorator.js";
export { decodeChallenge, pickAccept, acceptToPayment } from "./challenge.js";
export type { Accept, Challenge } from "./challenge.js";
export { canonicalize } from "./jcs.js";
