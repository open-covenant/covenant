// SAS plumbing for the er-verified registry: resolve the current attestation
// for a validator, and replace it (close + create) with a fresh, expiring one.
// The signer must be an authorized signer of the Covenant credential; the
// credential authority itself stays offline.
import {
  createSolanaRpc, createKeyPairSignerFromBytes, address, pipe,
  createTransactionMessage, setTransactionMessageFeePayerSigner,
  setTransactionMessageLifetimeUsingBlockhash, appendTransactionMessageInstructions,
  signTransactionMessageWithSigners, getBase64EncodedWireTransaction,
} from "@solana/kit";
import {
  deriveCredentialPda, deriveSchemaPda, deriveAttestationPda,
  getCreateAttestationInstruction, getCloseAttestationInstruction,
  fetchSchema, fetchMaybeAttestation, serializeAttestationData, deserializeAttestationData,
} from "sas-lib";
import fs from "node:fs";

const CRED_NAME = "covenant";
const SCHEMA_NAME = "er-verified";
const SCHEMA_VERSION = 1;

export const rpcFor = (url) => createSolanaRpc(url);
export const signerFromBytes = (bytes) => createKeyPairSignerFromBytes(Uint8Array.from(bytes));
export const signerFromEnv = () => {
  if (process.env.ER_MONITOR_KEY) return signerFromBytes(JSON.parse(process.env.ER_MONITOR_KEY));
  if (process.env.ER_MONITOR_KEY_FILE) return signerFromBytes(JSON.parse(fs.readFileSync(process.env.ER_MONITOR_KEY_FILE, "utf8")));
  throw new Error("set ER_MONITOR_KEY (json array) or ER_MONITOR_KEY_FILE");
};

async function send(rpc, signer, ixs) {
  const { value: bh } = await rpc.getLatestBlockhash().send();
  const msg = pipe(
    createTransactionMessage({ version: 0 }),
    (m) => setTransactionMessageFeePayerSigner(signer, m),
    (m) => setTransactionMessageLifetimeUsingBlockhash(bh, m),
    (m) => appendTransactionMessageInstructions(ixs, m),
  );
  const signed = await signTransactionMessageWithSigners(msg);
  const wire = getBase64EncodedWireTransaction(signed);
  const sig = await rpc.sendTransaction(wire, { encoding: "base64", preflightCommitment: "confirmed" }).send();
  for (let i = 0; i < 40; i++) {
    const st = (await rpc.getSignatureStatuses([sig]).send()).value[0];
    if (st?.confirmationStatus === "confirmed" || st?.confirmationStatus === "finalized") {
      if (st.err) throw new Error("tx failed: " + JSON.stringify(st.err));
      return sig;
    }
    await new Promise((r) => setTimeout(r, 1500));
  }
  throw new Error("confirm timeout " + sig);
}

export async function issuerPdas(issuer) {
  const [credential] = await deriveCredentialPda({ authority: address(issuer), name: CRED_NAME });
  const [schema] = await deriveSchemaPda({ credential, name: SCHEMA_NAME, version: SCHEMA_VERSION });
  return { credential, schema };
}

/** Current attestation for a validator, plus the schema account the refresh
 *  needs, so one call serves both the freshness check and the replace. */
export async function currentAttestation(rpc, issuer, validator) {
  const { credential, schema } = await issuerPdas(issuer);
  const [attestation] = await deriveAttestationPda({ credential, schema, nonce: address(validator) });
  const schemaAcct = await fetchSchema(rpc, schema);
  const acct = await fetchMaybeAttestation(rpc, attestation);
  const base = { credential, schema, schemaAcct, attestation };
  if (!acct.exists) return { ...base, exists: false };
  const d = deserializeAttestationData(schemaAcct.data, Uint8Array.from(acct.data.data));
  return {
    ...base,
    exists: true,
    expiry: Number(acct.data.expiry),
    mrTd: Buffer.from(d.mr_td).toString("hex"),
    status: d.status,
    endpoint: d.endpoint,
  };
}

/** Replace (or create) the attestation for a validator with a fresh one.
 *  `cur` is the currentAttestation result for the same validator. */
export async function refreshAttestation(rpc, signer, validator, v, ttlSeconds, cur) {
  const data = serializeAttestationData(cur.schemaAcct.data, {
    verified: v.status === "UpToDate",
    status: v.status,
    mr_td: Array.from(v.mrTd),
    verified_at: BigInt(v.verifiedAt),
    endpoint: v.endpoint,
  });
  const expiry = BigInt(v.verifiedAt + ttlSeconds);
  const ixs = [];
  if (cur.exists) {
    ixs.push(getCloseAttestationInstruction({ payer: signer, authority: signer, credential: cur.credential, attestation: cur.attestation }));
  }
  ixs.push(getCreateAttestationInstruction({
    payer: signer, authority: signer, credential: cur.credential, schema: cur.schema,
    attestation: cur.attestation, nonce: address(validator), data, expiry,
  }));
  const sig = await send(rpc, signer, ixs);
  return { attestation: cur.attestation, sig, expiry: Number(expiry) };
}
