// Covenant verified-ER resolver. Read-only: discovers MagicBlock ephemeral
// rollups from the Magic Router and checks each one against its Covenant
// attestation in the Solana Attestation Service (SAS), keyed to the validator
// identity the router already exposes. One account read per validator, no
// indexer, no keys.
//
// The attestation is published by the Covenant registry monitor after it
// DCAP-verifies the ER's TDX enclave. A resolver trusts it when the signer is
// an authorized signer of the Covenant credential and the attestation has not
// expired — expiry is the liveness signal: if the monitor stops re-verifying,
// attestations lapse and this library fails closed to unverified.
import { PublicKey } from "@solana/web3.js";
import { createHash } from "node:crypto";

export const SAS_PROGRAM = new PublicKey("22zoJMtdu4tQc2PzL74ZUT7FrwgB1Udec8DdW4yw4BdG");
export const COVENANT_ISSUER = new PublicKey("AdChcSmDKX57rU9qChMJ3MKnqNZbmiQAjuns9VCjzqRb");
export const SETTLEMENT_PROGRAM = new PublicKey("cov9UDypG7nsryxdgMcKhKU2spRVWLVjxT2iTv6do5Y");
export const MAGIC_ROUTER = "https://router.magicblock.app";

const CRED_NAME = "covenant";
const SCHEMA_NAME = "er-verified";
const SCHEMA_VERSION = 1;

const pda = (seeds) => PublicKey.findProgramAddressSync(seeds, SAS_PROGRAM)[0];

export function derivePdas(issuer = COVENANT_ISSUER) {
  const credential = pda([Buffer.from("credential"), issuer.toBuffer(), Buffer.from(CRED_NAME)]);
  const schema = pda([Buffer.from("schema"), credential.toBuffer(), Buffer.from(SCHEMA_NAME), Buffer.from([SCHEMA_VERSION])]);
  return { credential, schema };
}

export function deriveAttestationPda(validator, issuer = COVENANT_ISSUER) {
  const { credential, schema } = derivePdas(issuer);
  return pda([Buffer.from("attestation"), credential.toBuffer(), schema.toBuffer(), new PublicKey(validator).toBuffer()]);
}

/** ERs currently served by the Magic Router: [{identity, fqdn, countryCode, baseFee, blockTimeMs}]. */
export async function getRoutes(router = MAGIC_ROUTER) {
  const res = await fetch(router, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ jsonrpc: "2.0", id: 1, method: "getRoutes" }),
    signal: AbortSignal.timeout(10_000),
  });
  if (!res.ok) throw new Error(`${router} -> ${res.status}`);
  const routes = (await res.json())?.result;
  if (!Array.isArray(routes)) throw new Error("getRoutes returned no route list");
  return routes;
}

// Attestation account: u8 disc | nonce 32 | credential 32 | schema 32 |
// u32 data_len | data | signer 32 | expiry i64 LE | token_account 32.
// er-verified data (schema layout [bool, String, Vec<u8>, i64, String]):
// verified u8 | status | mr_td | verified_at i64 LE | endpoint.
function decodeAttestation(buf) {
  let o = 1 + 32 + 32 + 32;
  const dataLen = buf.readUInt32LE(o); o += 4;
  const data = buf.subarray(o, o + dataLen); o += dataLen;
  const signer = new PublicKey(buf.subarray(o, o + 32)); o += 32;
  const expiry = buf.readBigInt64LE(o);

  let d = 0;
  const verified = data[d] === 1; d += 1;
  const statusLen = data.readUInt32LE(d); d += 4;
  const status = data.subarray(d, d + statusLen).toString("utf8"); d += statusLen;
  const mrTdLen = data.readUInt32LE(d); d += 4;
  const mrTd = Buffer.from(data.subarray(d, d + mrTdLen)).toString("hex"); d += mrTdLen;
  const verifiedAt = Number(data.readBigInt64LE(d)); d += 8;
  const endpointLen = data.readUInt32LE(d); d += 4;
  const endpoint = data.subarray(d, d + endpointLen).toString("utf8");

  return { verified, status, mrTd, verifiedAt, endpoint, signer, expiry };
}

// Credential account: u8 disc | authority 32 | u32 name_len | name |
// u32 signer_count | signers 32 each.
function decodeAuthorizedSigners(buf) {
  let o = 1 + 32;
  const nameLen = buf.readUInt32LE(o); o += 4 + nameLen;
  // Clamp to what the buffer can actually hold so a corrupt length prefix
  // cannot spin the loop for billions of iterations.
  const count = Math.min(buf.readUInt32LE(o), Math.floor((buf.length - (o + 4)) / 32)); o += 4;
  const signers = [];
  for (let i = 0; i < count; i++, o += 32) signers.push(new PublicKey(buf.subarray(o, o + 32)));
  return signers;
}

/** Resolve one validator's Covenant attestation. Fails closed: any missing
 *  account, malformed data, untrusted signer, or lapsed expiry resolves to
 *  verified: false. */
export async function resolveVerifiedEr(connection, validator, { issuer = COVENANT_ISSUER } = {}) {
  const attestationPda = deriveAttestationPda(validator, issuer);
  const { credential } = derivePdas(issuer);
  const [attAcct, credAcct] = await connection.getMultipleAccountsInfo([attestationPda, credential]);
  if (!attAcct || !attAcct.owner.equals(SAS_PROGRAM)) {
    return { verified: false, reason: "no attestation for this validator", attestation: attestationPda };
  }
  if (!credAcct || !credAcct.owner.equals(SAS_PROGRAM)) {
    return { verified: false, reason: "issuer credential not found", attestation: attestationPda };
  }
  let att, signers;
  try {
    att = decodeAttestation(Buffer.from(attAcct.data));
    signers = decodeAuthorizedSigners(Buffer.from(credAcct.data));
  } catch (e) {
    return { verified: false, reason: `malformed attestation account (${e.message})`, attestation: attestationPda };
  }
  const trustedSigner = signers.some((s) => s.equals(att.signer));
  const notExpired = att.expiry === 0n || att.expiry > BigInt(Math.floor(Date.now() / 1000));
  const verified = trustedSigner && notExpired && att.verified;
  return {
    verified,
    reason: verified ? "verified" : !trustedSigner ? "attestation signer is not authorized by the issuer" : !notExpired ? "attestation expired (monitor lapsed)" : `enclave not verified (${att.status})`,
    attestation: attestationPda,
    trustedSigner,
    notExpired,
    status: att.status,
    mrTd: att.mrTd,
    verifiedAt: att.verifiedAt,
    endpoint: att.endpoint,
    expiry: Number(att.expiry),
    signer: att.signer.toBase58(),
  };
}

/** Discover ERs from the router and annotate each with its Covenant verification.
 *  `picked` is the first verified ER, or null when none verify. */
export async function pickVerifiedEr(connection, { router = MAGIC_ROUTER, issuer = COVENANT_ISSUER } = {}) {
  const routes = await getRoutes(router);
  const annotated = [];
  for (const r of routes) {
    try {
      annotated.push({ ...r, covenant: await resolveVerifiedEr(connection, r.identity, { issuer }) });
    } catch (e) {
      // One bad route entry (invalid identity, RPC hiccup) must not discard
      // the rest of the listing.
      annotated.push({ ...r, covenant: { verified: false, reason: e.message, attestation: null } });
    }
  }
  return { picked: annotated.find((r) => r.covenant.verified) ?? null, routes: annotated };
}

const sha256 = (...parts) => createHash("sha256").update(Buffer.concat(parts)).digest();

/** Fold receipt hashes the way consume_credits does on chain:
 *  root = sha256(root || receipt_hash), genesis = 32 zero bytes. */
export function foldProvenance(receiptHashes) {
  let root = Buffer.alloc(32);
  for (const h of receiptHashes) {
    const buf = typeof h === "string" ? Buffer.from(h, "hex") : Buffer.from(h);
    if (buf.length !== 32) throw new Error("receipt hashes must be 32 bytes");
    root = sha256(root, buf);
  }
  return root;
}

// CreditAccount: 8 disc | owner 32 | balance u64 | bump 1 | provenance_root 32.
const PROVENANCE_OFFSET = 49;

/** Recompute a provenance root from receipt hashes and compare it against the
 *  root stored in the agent's credit account on chain. */
export async function verifyProvenanceRoot(connection, creditsAccount, receiptHashes) {
  const acct = await connection.getAccountInfo(new PublicKey(creditsAccount));
  if (!acct) throw new Error("credit account not found");
  if (!acct.owner.equals(SETTLEMENT_PROGRAM)) throw new Error("account is not owned by the Covenant settlement program");
  if (acct.data.length < PROVENANCE_OFFSET + 32) throw new Error("credit account predates provenance (not migrated)");
  const onChain = Buffer.from(acct.data.subarray(PROVENANCE_OFFSET, PROVENANCE_OFFSET + 32));
  const recomputed = foldProvenance(receiptHashes);
  return { match: recomputed.equals(onChain), onChain: onChain.toString("hex"), recomputed: recomputed.toString("hex") };
}
