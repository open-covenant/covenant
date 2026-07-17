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

/** Current attestation for a validator: { exists, expiry, mrTd, status } */
export async function currentAttestation(rpc, issuer, validator) {
  const { credential, schema } = await issuerPdas(issuer);
  const [attestation] = await deriveAttestationPda({ credential, schema, nonce: address(validator) });
  const acct = await fetchMaybeAttestation(rpc, attestation);
  if (!acct.exists) return { attestation, exists: false };
  const d = deserializeAttestationData(await fetchSchema(rpc, schema).then((s) => s.data), Uint8Array.from(acct.data.data));
  return {
    attestation,
    exists: true,
    expiry: Number(acct.data.expiry),
    mrTd: Buffer.from(d.mr_td).toString("hex"),
    status: d.status,
    signer: acct.data.signer,
  };
}

/** Replace (or create) the attestation for a validator with a fresh one. */
export async function refreshAttestation(rpc, signer, issuer, validator, v, ttlSeconds) {
  const { credential, schema } = await issuerPdas(issuer);
  const nonce = address(validator);
  const [attestation] = await deriveAttestationPda({ credential, schema, nonce });
  const schemaAcct = await fetchSchema(rpc, schema);
  const data = serializeAttestationData(schemaAcct.data, {
    verified: v.status === "UpToDate",
    status: v.status,
    mr_td: Array.from(v.mrTd),
    verified_at: BigInt(v.verifiedAt),
    endpoint: v.endpoint,
  });
  const expiry = BigInt(v.verifiedAt + ttlSeconds);
  const ixs = [];
  if ((await fetchMaybeAttestation(rpc, attestation)).exists) {
    ixs.push(getCloseAttestationInstruction({ payer: signer, authority: signer, credential, attestation }));
  }
  ixs.push(getCreateAttestationInstruction({ payer: signer, authority: signer, credential, schema, attestation, nonce, data, expiry }));
  const sig = await send(rpc, signer, ixs);
  return { attestation, sig, expiry: Number(expiry) };
}
